from __future__ import annotations

import os
import shutil
import subprocess

from .base import AdapterRequest, AdapterResponse, BaseAdapter, ErrorCode
from .context import build_augmented_prompt

RATE_LIMIT_MARKERS = (
    "rate limit",
    "rate_limit",
    "429",
    "too many requests",
    "quota",
    "usage limit",
    "overloaded",
)
AUTH_MARKERS = (
    "unauthorized",
    "authentication",
    "api key",
    "invalid_api_key",
    "401",
)


class VibeAdapter(BaseAdapter):
    """Headless subprocess wrapper for Mistral Vibe CLI (`vibe`)."""

    name = "vibe"

    def execute(self, request: AdapterRequest) -> AdapterResponse:
        binary = request.command or "vibe"
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

        cmd = [binary, "--prompt", full_prompt, "--output", "text"]
        if request.working_dir and "--workdir" not in request.args:
            cmd.extend(["--workdir", request.working_dir])
        if request.readonly and "--agent" not in request.args:
            cmd.extend(["--agent", "ask"])
        elif not request.readonly and "--agent" not in request.args:
            cmd.extend(["--agent", "accept-edits"])
        cmd.extend(request.args)

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
        except subprocess.TimeoutExpired:
            return AdapterResponse(
                status=ErrorCode.TIMEOUT,
                error_message=f"Mistral Vibe CLI timed out after {request.timeout_secs}s",
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
            if any(m in combined_lower for m in RATE_LIMIT_MARKERS):
                return AdapterResponse(
                    status=ErrorCode.RATE_LIMITED,
                    error_message=err_text,
                    peer=request.peer,
                    adapter=self.name,
                )
            if any(m in combined_lower for m in AUTH_MARKERS):
                return AdapterResponse(
                    status=ErrorCode.AUTH_ERROR,
                    error_message=err_text,
                    peer=request.peer,
                    adapter=self.name,
                )
            return AdapterResponse(
                status=ErrorCode.EXEC_ERROR,
                error_message=err_text or f"Exited with code {proc.returncode}",
                peer=request.peer,
                adapter=self.name,
            )

        return AdapterResponse(
            status=ErrorCode.OK,
            output=proc.stdout.strip(),
            peer=request.peer,
            adapter=self.name,
        )
