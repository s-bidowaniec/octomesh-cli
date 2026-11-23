from __future__ import annotations

import math
import os
import shutil
import stat
import subprocess
import tempfile
import time
import unicodedata
from pathlib import Path

from .base import AdapterRequest, AdapterResponse, BaseAdapter, ErrorCode
from .context import build_augmented_prompt

DONE_MARKER = "<<OCTO_DONE>>"
MAX_REPLY_BYTES = 64 * 1024
KONSOLE_INTERFACE = "org.kde.konsole.Session"
KONSOLE_PASTE = f"{KONSOLE_INTERFACE}.sendText"
KONSOLE_NAMESPACE = "org.kde.konsole"

NOT_FOUND_MARKERS = (
    "serviceunknown",
    "namehasnoowner",
    "no such object path",
    "no such interface",
    "unknown method",
    "not found",
)
RATE_LIMIT_MARKERS = (
    "rate limit",
    "rate_limit",
    "429",
    "too many requests",
    "quota",
    "overloaded",
)
AUTH_MARKERS = (
    "unauthorized",
    "authentication",
    "access denied",
    "permission denied",
    "401",
    "403",
)


def _parse_kv(args: list[str]) -> dict[str, str]:
    cfg: dict[str, str] = {}
    for item in args:
        if "=" in item:
            key, value = item.split("=", 1)
            cfg[key.strip()] = value.strip()
    return cfg


def _gvariant_string(text: str) -> str:
    escaped = (
        text.replace("\\", "\\\\")
        .replace("'", "\\'")
        .replace("\n", "\\n")
        .replace("\r", "\\r")
        .replace("\t", "\\t")
    )
    return f"'{escaped}'"


def _terminal_line(text: str) -> str:
    printable = "".join(
        char for char in text if char.isspace() or unicodedata.category(char) not in {"Cc", "Cf"}
    )
    return " ".join(printable.split())


def _create_reply_path(working_dir: str, configured_path: str) -> Path:
    root = Path(working_dir).resolve()
    relative = Path(configured_path)
    if relative.is_absolute() or ".." in relative.parts or not relative.name:
        raise ValueError("reply_file must be a relative file path inside the working directory")
    parent = (root / relative.parent).resolve()
    if not parent.is_relative_to(root):
        raise ValueError("reply_file must stay inside the working directory")
    parent.mkdir(parents=True, exist_ok=True)
    private_dir = Path(tempfile.mkdtemp(prefix="reply-", dir=parent))
    return private_dir / relative.name


def _read_reply(path: Path) -> str | None:
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except FileNotFoundError:
        return None
    with os.fdopen(fd, "rb") as reply:
        if not stat.S_ISREG(os.fstat(reply.fileno()).st_mode):
            raise ValueError("reply must be a regular file")
        data = reply.read(MAX_REPLY_BYTES + 1)
    if len(data) > MAX_REPLY_BYTES:
        raise ValueError(f"reply exceeds {MAX_REPLY_BYTES} bytes")
    lines = data.decode("utf-8", errors="replace").rstrip().splitlines()
    if lines and lines[-1] == DONE_MARKER:
        return "\n".join(lines[:-1]).strip() or "(empty reply)"
    return None


class DbusWindowAdapter(BaseAdapter):
    """Pass a prompt to an existing session using the local D-Bus session bus."""

    name = "dbus"

    def execute(self, request: AdapterRequest) -> AdapterResponse:
        cfg = _parse_kv(request.args)
        bus = cfg.get("bus", "")
        path = cfg.get("path", "")
        method = cfg.get("method", "")
        if not bus or not path or not method:
            return AdapterResponse(
                status=ErrorCode.EXEC_ERROR,
                error_message="dbus peer requires args: bus=..., path=..., method=...",
                peer=request.peer,
                adapter=self.name,
            )
        konsole = bus.startswith(KONSOLE_NAMESPACE) or method.startswith(f"{KONSOLE_NAMESPACE}.")
        if konsole and (method != KONSOLE_PASTE or cfg.get("allow_terminal_paste") != "true"):
            return AdapterResponse(
                status=ErrorCode.EXEC_ERROR,
                error_message="Konsole requires sendText and allow_terminal_paste=true",
            )

        binary = request.command or cfg.get("exec") or ""
        if not binary:
            binary = next((b for b in ("gdbus", "busctl") if shutil.which(b)), "")
        if not binary or shutil.which(binary) is None:
            return AdapterResponse(
                status=ErrorCode.CLI_NOT_FOUND,
                error_message="Neither gdbus nor busctl found on PATH; set command= in peer config",
                peer=request.peer,
                adapter=self.name,
            )

        reply_file = cfg.get("reply_file", "")
        reply_path = None
        try:
            poll_secs = float(cfg.get("poll_secs", "2"))
            if not math.isfinite(poll_secs) or poll_secs <= 0:
                raise ValueError("poll_secs must be a finite positive number")
            if reply_file:
                reply_path = _create_reply_path(request.working_dir, reply_file)
        except (ValueError, OSError, RuntimeError) as exc:
            return AdapterResponse(status=ErrorCode.EXEC_ERROR, error_message=str(exc))

        prompt = request.prompt
        if reply_path is not None:
            prompt += (
                f"\n\nWrite your final answer to the file `{reply_path}` and end the file "
                f"with a line consisting solely of `{DONE_MARKER}`."
            )
        full_prompt = build_augmented_prompt(
            prompt,
            caller=request.caller,
            depth=request.depth,
            readonly=request.readonly,
            include_diff=request.include_diff,
            working_dir=request.working_dir,
            task_id=request.env.get("OCTOMESH_TASK_ID"),
        )
        if konsole:
            full_prompt = _terminal_line(full_prompt)
        elif "\0" in full_prompt:
            return AdapterResponse(
                status=ErrorCode.EXEC_ERROR, error_message="D-Bus strings cannot contain NUL"
            )

        is_busctl = os.path.basename(binary).startswith("busctl")
        if is_busctl:
            iface, _, member = method.rpartition(".")
            cmd = [
                binary,
                "--user",
                "call",
                bus,
                path,
                iface,
                member,
                "s",
                full_prompt,
            ]
        else:
            cmd = [
                binary,
                "call",
                "--session",
                "--dest",
                bus,
                "--object-path",
                path,
                "--method",
                method,
                _gvariant_string(full_prompt),
            ]

        env = {**os.environ, **request.env}
        start = time.monotonic()
        deadline = start + request.timeout_secs
        try:
            proc = subprocess.run(
                cmd,
                cwd=request.working_dir or None,
                env=env,
                capture_output=True,
                text=True,
                timeout=request.timeout_secs,
                check=False,
            )
        except subprocess.TimeoutExpired:
            return AdapterResponse(
                status=ErrorCode.TIMEOUT,
                error_message=f"D-Bus call to '{bus}' timed out; delivery is unknown",
                peer=request.peer,
                adapter=self.name,
            )
        except OSError as exc:
            return AdapterResponse(
                status=ErrorCode.EXEC_ERROR,
                error_message=str(exc),
                peer=request.peer,
                adapter=self.name,
            )

        combined_lower = f"{proc.stdout}\n{proc.stderr}".lower()
        if proc.returncode != 0:
            err_text = (proc.stderr or proc.stdout).strip()
            if any(m in combined_lower for m in NOT_FOUND_MARKERS):
                status = ErrorCode.CLI_NOT_FOUND
            elif any(m in combined_lower for m in RATE_LIMIT_MARKERS):
                status = ErrorCode.RATE_LIMITED
            elif any(m in combined_lower for m in AUTH_MARKERS):
                status = ErrorCode.AUTH_ERROR
            else:
                status = ErrorCode.EXEC_ERROR
            return AdapterResponse(
                status=status,
                error_message=err_text or f"Exited with code {proc.returncode}",
                peer=request.peer,
                adapter=self.name,
            )

        if reply_path is None:
            output = (
                "D-Bus accepted sendText. Check the target terminal and submit the prompt there."
                if konsole
                else "D-Bus method returned successfully. No agent reply was requested."
            )
            return AdapterResponse(
                status=ErrorCode.OK,
                output=output,
                peer=request.peer,
                adapter=self.name,
            )

        while True:
            try:
                output = _read_reply(reply_path)
            except (ValueError, OSError) as exc:
                return AdapterResponse(
                    status=ErrorCode.EXEC_ERROR,
                    error_message=f"D-Bus call succeeded but could not read its reply: {exc}",
                )
            if output is not None:
                return AdapterResponse(status=ErrorCode.OK, output=output)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            time.sleep(min(max(0.2, poll_secs), remaining))

        return AdapterResponse(
            status=ErrorCode.TIMEOUT,
            error_message=(
                f"D-Bus call to '{bus}{path}' succeeded; no completed reply with {DONE_MARKER} "
                f"at '{reply_path}' within {request.timeout_secs}s"
            ),
            peer=request.peer,
            adapter=self.name,
        )
