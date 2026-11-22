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
    "resource_exhausted",
    "exhausted",
    "capacity",
    "overloaded",
    "weekly limit",
    "usage limit",
)
AUTH_MARKERS = (
    "unauthorized",
    "unauthenticated",
    "authentication",
    "not logged in",
    "login required",
    "api key",
    "401",
)


class AgyAdapter(BaseAdapter):
    """Headless subprocess wrapper for Google Antigravity CLI (`agy --print`)."""

    name = "agy"

    def execute(self, request: AdapterRequest) -> AdapterResponse:
        binary = request.command or "agy"
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

        cmd = [
            binary,
            "--print",
            full_prompt,
            "--output-format",
            "text",
            "--disable-slash-commands",
            "--print-timeout",
            f"{request.timeout_secs}s",
            *request.args,
        ]
        if "--mode" not in cmd:
            cmd.extend(["--mode", "plan" if request.readonly else "auto"])

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
                error_message=f"Antigravity CLI timed out after {request.timeout_secs}s",
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
