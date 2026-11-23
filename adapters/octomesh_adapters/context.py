from __future__ import annotations

import subprocess
from pathlib import Path

MAX_DIFF_CHARS = 12_000


def capture_git_context(working_dir: str, max_chars: int = MAX_DIFF_CHARS) -> str:
    """Safely capture `git status --short` and a truncated `git diff` if inside a git repo."""
    cwd = Path(working_dir) if working_dir else Path.cwd()
    if not cwd.exists():
        return ""

    try:
        status_proc = subprocess.run(
            ["git", "status", "--short"],
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        if status_proc.returncode != 0:
            return ""

        diff_proc = subprocess.run(
            ["git", "diff", "HEAD"],
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return ""

    status_out = status_proc.stdout.strip()
    diff_out = diff_proc.stdout.strip()

    if not status_out and not diff_out:
        return ""

    if len(diff_out) > max_chars:
        diff_out = diff_out[:max_chars] + "\n... [diff truncated by octomesh] ..."

    sections = ["--- GIT CONTEXT ---"]
    if status_out:
        sections.append(f"Status:\n{status_out}")
    if diff_out:
        sections.append(f"Diff:\n{diff_out}")
    sections.append("--- END GIT CONTEXT ---")
    return "\n".join(sections)


def build_delegation_header(
    caller: str,
    depth: int,
    readonly: bool,
    task_id: str | None = None,
) -> str:
    """Typed header line; includes `task=<id>` when the router set OCTOMESH_TASK_ID."""
    parts = ["Octomesh Delegation"]
    if task_id and task_id.strip():
        parts.append(f"task={task_id.strip()}")
    parts.append(f"caller={caller}")
    parts.append(f"depth={depth}")
    parts.append(f"readonly={readonly}")
    return "[" + " | ".join(parts) + "]"


def build_augmented_prompt(
    prompt: str,
    *,
    caller: str,
    depth: int,
    readonly: bool,
    include_diff: bool,
    working_dir: str,
    task_id: str | None = None,
) -> str:
    parts = [
        build_delegation_header(caller, depth, readonly, task_id),
    ]
    if readonly:
        parts.append(
            "CONSTRAINT: Operate in STRICT READ-ONLY mode. "
            "Do NOT modify, create, or delete any files."
        )
    parts.append(prompt)

    if include_diff:
        git_ctx = capture_git_context(working_dir)
        if git_ctx:
            parts.append(git_ctx)

    return "\n\n".join(parts)
