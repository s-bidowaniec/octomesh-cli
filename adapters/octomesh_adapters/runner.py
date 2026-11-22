from __future__ import annotations

import json
import sys
import time

from .agy import AgyAdapter
from .base import AdapterRequest, AdapterResponse, BaseAdapter, ErrorCode
from .claude import ClaudeAdapter
from .codex import CodexAdapter
from .mock import MockAdapter
from .vibe import VibeAdapter

ADAPTER_REGISTRY: dict[str, type[BaseAdapter]] = {
    "mock": MockAdapter,
    "claude": ClaudeAdapter,
    "vibe": VibeAdapter,
    "codex": CodexAdapter,
    "agy": AgyAdapter,
}


def dispatch(raw_payload: str) -> AdapterResponse:
    start = time.monotonic()
    try:
        data = json.loads(raw_payload)
    except json.JSONDecodeError as exc:
        return AdapterResponse(
            status=ErrorCode.EXEC_ERROR,
            error_message=f"Invalid JSON input: {exc}",
        )

    req = AdapterRequest.from_dict(data)
    adapter_cls = ADAPTER_REGISTRY.get(req.adapter)
    if adapter_cls is None:
        return AdapterResponse(
            status=ErrorCode.CLI_NOT_FOUND,
            error_message=f"Unknown adapter '{req.adapter}'",
            peer=req.peer,
            adapter=req.adapter,
        )

    adapter = adapter_cls()
    resp = adapter.execute(req)
    resp.peer = req.peer
    resp.adapter = req.adapter
    resp.duration_ms = int((time.monotonic() - start) * 1000)
    return resp


def main() -> None:
    raw = sys.stdin.read()
    resp = dispatch(raw)
    sys.stdout.write(json.dumps(resp.to_dict()) + "\n")
    sys.stdout.flush()


if __name__ == "__main__":
    main()
