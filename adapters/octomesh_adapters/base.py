from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class ErrorCode(str, Enum):
    OK = "OK"
    RATE_LIMITED = "RATE_LIMITED"
    TIMEOUT = "TIMEOUT"
    AUTH_ERROR = "AUTH_ERROR"
    CLI_NOT_FOUND = "CLI_NOT_FOUND"
    EXEC_ERROR = "EXEC_ERROR"


@dataclass
class AdapterRequest:
    peer: str
    adapter: str
    prompt: str
    caller: str = "unknown"
    depth: int = 0
    include_diff: bool = False
    readonly: bool = False
    timeout_secs: int = 120
    command: str | None = None
    args: list[str] = field(default_factory=list)
    env: dict[str, str] = field(default_factory=dict)
    working_dir: str = "."

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> AdapterRequest:
        return cls(
            peer=str(data.get("peer", "unknown")),
            adapter=str(data.get("adapter", "mock")),
            prompt=str(data.get("prompt", "")),
            caller=str(data.get("caller", "unknown")),
            depth=int(data.get("depth", 0)),
            include_diff=bool(data.get("include_diff", False)),
            readonly=bool(data.get("readonly", False)),
            timeout_secs=int(data.get("timeout_secs", 120)),
            command=data.get("command"),
            args=list(data.get("args") or []),
            env=dict(data.get("env") or {}),
            working_dir=str(data.get("working_dir", ".")),
        )


@dataclass
class AdapterResponse:
    status: ErrorCode
    output: str = ""
    error_message: str | None = None
    peer: str = ""
    adapter: str = ""
    duration_ms: int = 0

    def to_dict(self) -> dict[str, Any]:
        return {
            "status": self.status.value,
            "output": self.output,
            "error_message": self.error_message,
            "peer": self.peer,
            "adapter": self.adapter,
            "duration_ms": self.duration_ms,
        }


class BaseAdapter(ABC):
    name: str = "base"

    @abstractmethod
    def execute(self, request: AdapterRequest) -> AdapterResponse:
        """Execute a prompt against the underlying terminal AI CLI."""
