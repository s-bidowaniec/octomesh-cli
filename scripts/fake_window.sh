#!/usr/bin/env bash
# AF-08 helper: fake "window" command for `octo` e2e --mock-only mode.
# Logs argv, records the task id header, writes the reply file, prints a GVariant tuple.
set -euo pipefail

LOG_DIR="${OCTOMESH_FAKE_WINDOW_DIR:-${TMPDIR:-/tmp}/octomesh_fake_window}"
mkdir -p "$LOG_DIR"

printf '%s\n' "$*" >>"$LOG_DIR/calls.log"
if [[ -n "${OCTOMESH_TASK_ID:-}" ]]; then
  printf 'task=%s\n' "$OCTOMESH_TASK_ID" >>"$LOG_DIR/tasks.log"
fi
if [[ -n "${OCTOMESH_REPLY_FILE:-}" ]]; then
  printf '<<OCTO_DONE>>\n' >"$OCTOMESH_REPLY_FILE"
fi
printf '(true,)\n'
