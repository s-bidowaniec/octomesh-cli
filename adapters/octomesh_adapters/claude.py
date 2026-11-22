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
    "overloaded",
    "quota exceeded",
    "quota",
    "too many requests",
    "weekly limit",
    "usage limit",
    "out of usage credits",
    "hit your limit",
    "credit balance",
)
AUTH_MARKERS = (
    "unauthorized",
    "authentication",
    "api key",
    "invalid_api_key",
    "not logged in",
    "401",
)


class ClaudeAdapter(BaseAdapter):
    """Headless subprocess wrapper for Claude Code CLI (`claude -p`)."""

    name = "claude"

    def execute(self, request: AdapterRequest) -> AdapterResponse:
        binary = request.command or "claude"
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

        cmd = [binary, "-p", full_prompt, *request.args]
        if request.readonly:
            if "--allowedTools" not in cmd and "--allowed-tools" not in cmd:
                cmd.extend(["--allowedTools", "Read,Grep,Glob"])
            if "--permission-mode" not in cmd:
                cmd.extend(["--permission-mode", "plan"])

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
                error_message=f"Claude CLI timed out after {request.timeout_secs}s",
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
            err_text = proc.stderr.strip() or proc.stdout.strip()
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
