from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
from pathlib import Path

from .base import AdapterRequest, AdapterResponse, BaseAdapter, ErrorCode
from .context import build_augmented_prompt

RATE_LIMIT_MARKERS = (
    "rate limit",
    "rate_limit",
    "429",
    "too many requests",
    "quota",
    "usage limit",
    "insufficient_quota",
    "overloaded",
)
AUTH_MARKERS = (
    "unauthorized",
    "authentication",
    "not logged in",
    "invalid_api_key",
    "401",
)


class CodexAdapter(BaseAdapter):
    """Headless subprocess wrapper for OpenAI Codex CLI (`codex exec`)."""

    name = "codex"

    def execute(self, request: AdapterRequest) -> AdapterResponse:
        binary = request.command or "codex"
        if shutil.which(binary) is None:
            return AdapterResponse(
                status=ErrorCode.CLI_NOT_FOUND,
                error_message=f"CLI binary '{binary}' not found on PATH",
                peer=request.peer,
                adapter=self.name,
            )

        full_prompt = build_augmented_prompt(
            request.prompt,
            caller=request.caller,
            depth=request.depth,
            readonly=request.readonly,
            include_diff=request.include_diff,
            working_dir=request.working_dir,
        )

        with tempfile.NamedTemporaryFile(prefix="octo_codex_", suffix=".txt", delete=False) as tmp:
            out_path = Path(tmp.name)

        cmd = [
            binary,
            "exec",
            "--ephemeral",
            "--skip-git-repo-check",
            "-o",
            str(out_path),
        ]
        if request.working_dir:
            cmd.extend(["-C", request.working_dir])

        has_sandbox = any(a in ("-s", "--sandbox") for a in request.args)
        if not has_sandbox:
            cmd.extend(["-s", "read-only" if request.readonly else "workspace-write"])

        cmd.extend(request.args)
        cmd.append(full_prompt)

        env = {**os.environ, **request.env}
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
            last_msg = out_path.read_text(encoding="utf-8").strip() if out_path.exists() else ""
        except subprocess.TimeoutExpired:
            return AdapterResponse(
                status=ErrorCode.TIMEOUT,
                error_message=f"Codex CLI timed out after {request.timeout_secs}s",
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
        finally:
            out_path.unlink(missing_ok=True)

        combined_lower = f"{proc.stdout}\n{proc.stderr}\n{last_msg}".lower()
        if proc.returncode != 0:
            err = (proc.stderr or proc.stdout or last_msg).strip()
            if any(m in combined_lower for m in RATE_LIMIT_MARKERS):
                status = ErrorCode.RATE_LIMITED
            elif any(m in combined_lower for m in AUTH_MARKERS):
                status = ErrorCode.AUTH_ERROR
            else:
                status = ErrorCode.EXEC_ERROR
            return AdapterResponse(
                status=status,
                error_message=err or f"Exited with code {proc.returncode}",
                peer=request.peer,
                adapter=self.name,
            )

        return AdapterResponse(
            status=ErrorCode.OK,
            output=last_msg or proc.stdout.strip(),
            peer=request.peer,
            adapter=self.name,
        )
