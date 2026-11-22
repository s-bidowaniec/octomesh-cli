from __future__ import annotations

from .base import AdapterRequest, AdapterResponse, BaseAdapter, ErrorCode
from .context import build_augmented_prompt


class MockAdapter(BaseAdapter):
    """Deterministic test adapter supporting simulated failure flags in prompts or env.

    Flags (triggered only on `mock_primary` or when `MOCK_FORCE_ERROR` is set):
      - `[SIMULATE_RATE_LIMIT]` -> returns `RATE_LIMITED`
      - `[SIMULATE_TIMEOUT]`    -> returns `TIMEOUT`
      - `[SIMULATE_AUTH_ERROR]` -> returns `AUTH_ERROR`
    """

    name = "mock"

    def execute(self, request: AdapterRequest) -> AdapterResponse:
        forced = request.env.get("MOCK_FORCE_ERROR", "").upper()
        is_primary = request.peer.endswith("primary")

        if forced == "RATE_LIMITED" or (is_primary and "[SIMULATE_RATE_LIMIT]" in request.prompt):
            return AdapterResponse(
                status=ErrorCode.RATE_LIMITED,
                error_message=f"Simulated HTTP 429 rate limit on peer '{request.peer}'",
                peer=request.peer,
                adapter=self.name,
            )

        if forced == "TIMEOUT" or (is_primary and "[SIMULATE_TIMEOUT]" in request.prompt):
            return AdapterResponse(
                status=ErrorCode.TIMEOUT,
                error_message=f"Simulated execution timeout on peer '{request.peer}'",
                peer=request.peer,
                adapter=self.name,
            )

        if forced == "AUTH_ERROR" or (is_primary and "[SIMULATE_AUTH_ERROR]" in request.prompt):
            return AdapterResponse(
                status=ErrorCode.AUTH_ERROR,
                error_message=f"Simulated authentication failure on peer '{request.peer}'",
                peer=request.peer,
                adapter=self.name,
            )

        augmented = build_augmented_prompt(
            request.prompt,
            caller=request.caller,
            depth=request.depth,
            readonly=request.readonly,
            include_diff=request.include_diff,
            working_dir=request.working_dir,
        )

        return AdapterResponse(
            status=ErrorCode.OK,
            output=f"[{request.peer} via mock] Processed prompt:\n{augmented}",
            peer=request.peer,
            adapter=self.name,
        )
