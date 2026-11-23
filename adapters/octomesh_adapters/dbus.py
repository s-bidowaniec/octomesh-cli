from __future__ import annotations

import os
import shutil
import subprocess
import time
from pathlib import Path

from .base import AdapterRequest, AdapterResponse, BaseAdapter, ErrorCode
from .context import build_augmented_prompt

DONE_MARKER = "<<OCTO_DONE>>"

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
    escaped = text.replace("\\", "\\\\").replace("'", "\\'")
    return f"'{escaped}'"


class DbusWindowAdapter(BaseAdapter):
    """Types a message into a live terminal window over the D-Bus session bus.

    Replaces ad-hoc per-terminal scripts: the peer is an already open terminal
    window (e.g. KDE Konsole ``org.kde.konsole.Session.sendText`` or a custom
    helper service). The target agent answers inside its existing conversation,
    so no prompt/context is re-sent (token-effective). Optional ``reply_file``
    is the return channel: the target writes its answer there and ends the
    file with a line containing DONE_MARKER.

    Peer args (key=value in octomesh.toml [peers.<name>].args):
      bus=org.kde.konsole   well-known bus name
      path=/Sessions/2      object path of the window/session
      method=org.kde.konsole.Session.sendText   full D-Bus method
      exec=gdbus|busctl     caller binary (default: first of gdbus, busctl)
      reply_file=.octomesh/inbox/<peer>.md      optional reply inbox
      poll_secs=2           reply polling interval
    """

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
        reply_path = Path(request.working_dir) / reply_file if reply_file else None
        if reply_path is not None:
            reply_path.parent.mkdir(parents=True, exist_ok=True)
            reply_path.unlink(missing_ok=True)

        prompt = request.prompt
        if reply_path is not None:
            prompt += (
                f"\n\nWrite your final answer to the file `{reply_file}` and end the file "
                f"with a line containing `{DONE_MARKER}`."
            )
        full_prompt = build_augmented_prompt(
            prompt,
            caller=request.caller,
            depth=request.depth,
            readonly=request.readonly,
            include_diff=request.include_diff,
            working_dir=request.working_dir,
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
                timeout=max(1, int(deadline - time.monotonic())),
                check=False,
            )
        except subprocess.TimeoutExpired:
            return AdapterResponse(
                status=ErrorCode.TIMEOUT,
                error_message=f"D-Bus call to '{bus}' timed out after {request.timeout_secs}s",
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
            return AdapterResponse(
                status=ErrorCode.OK,
                output=proc.stdout.strip(),
                peer=request.peer,
                adapter=self.name,
            )

        poll_secs = max(0.2, float(cfg.get("poll_secs", "2")))
        while time.monotonic() < deadline:
            if reply_path.exists():
                content = reply_path.read_text(encoding="utf-8", errors="replace")
                if DONE_MARKER in content:
                    output = content.split(DONE_MARKER, 1)[0].strip()
                    return AdapterResponse(
                        status=ErrorCode.OK,
                        output=output or "(empty reply)",
                        peer=request.peer,
                        adapter=self.name,
                    )
            time.sleep(min(poll_secs, max(0.1, deadline - time.monotonic())))

        return AdapterResponse(
            status=ErrorCode.TIMEOUT,
            error_message=(
                f"Message typed into '{bus}{path}', but no reply with {DONE_MARKER} "
                f"in {reply_file} within {request.timeout_secs}s"
            ),
            peer=request.peer,
            adapter=self.name,
        )
