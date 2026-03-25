"""Direct Python worker hook entrypoints and settings generation."""

from __future__ import annotations

import json
import os
import sys
import time
from pathlib import Path
from typing import Any

from singleton import store


def generate_settings(thread_id: str, run_id: str, state_dir: Path) -> str:
    """Generate `--settings` JSON for worker spawn."""

    python = sys.executable

    def hook_cmd(name: str) -> str:
        return (
            f"SINGLETON_THREAD_ID={thread_id} "
            f"SINGLETON_RUN_ID={run_id} "
            f"SINGLETON_STATE_DIR={state_dir} "
            f"SINGLETON_DB_PATH={state_dir / 'messages.db'} "
            f"{python} -m singleton.hooks {name}"
        )

    settings = {
        "hooks": {
            "SessionStart": [
                {"hooks": [{"type": "command", "command": hook_cmd("session-start")}]}
            ],
            "PermissionRequest": [
                {
                    "matcher": "*",
                    "hooks": [
                        {"type": "command", "command": hook_cmd("permission-request")}
                    ],
                }
            ],
            "Stop": [{"hooks": [{"type": "command", "command": hook_cmd("stop")}]}],
            "StopFailure": [
                {
                    "matcher": "*",
                    "hooks": [{"type": "command", "command": hook_cmd("stop-failure")}],
                }
            ],
            "Notification": [
                {
                    "matcher": "*",
                    "hooks": [{"type": "command", "command": hook_cmd("notification")}],
                }
            ],
        }
    }
    return json.dumps(settings)


def main(argv: list[str] | None = None) -> int:
    argv = argv or sys.argv[1:]
    if len(argv) != 1:
        raise SystemExit("usage: python -m singleton.hooks <hook-name>")

    _configure_store_from_env()
    hook_name = argv[0]
    payload = json.load(sys.stdin)

    handlers = {
        "session-start": handle_session_start,
        "permission-request": handle_permission_request,
        "stop": handle_stop,
        "stop-failure": handle_stop_failure,
        "notification": handle_notification,
    }
    try:
        handler = handlers[hook_name]
    except KeyError as exc:
        raise SystemExit(f"unknown hook name: {hook_name}") from exc
    return handler(payload)


def handle_session_start(payload: dict[str, Any]) -> int:
    store.append_message(
        direction="from_worker",
        message_type="run_started",
        thread_id=_thread_id(),
        run_id=_run_id(),
        payload={
            "session_id": payload["session_id"],
            "source": payload.get("source", "startup"),
        },
    )
    return 0


def handle_permission_request(payload: dict[str, Any]) -> int:
    request_id = payload.get("request_id") or store.generate_message_id().replace(
        "msg_", "req_"
    )
    store.append_message(
        direction="from_worker",
        message_type="permission_request",
        thread_id=_thread_id(),
        run_id=_run_id(),
        payload={
            "request_id": request_id,
            "tool_name": payload["tool_name"],
            "tool_input": payload.get("tool_input", {}),
            "permission_mode": payload.get("permission_mode")
            or payload.get("permission_mode", "default"),
            "permission_suggestions": payload.get("permission_suggestions", []),
        },
    )
    resolution = _wait_for_permission_resolution(request_id)
    if resolution is None:
        print("permission request timed out", file=sys.stderr)
        return 2
    decision = {
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": {
                "behavior": resolution["decision"],
            },
        }
    }
    reason = resolution.get("reason")
    if resolution["decision"] == "deny" and reason:
        decision["hookSpecificOutput"]["decision"]["message"] = reason
    print(json.dumps(decision))
    return 0


def handle_stop(payload: dict[str, Any]) -> int:
    store.append_message(
        direction="from_worker",
        message_type="run_finished",
        thread_id=_thread_id(),
        run_id=_run_id(),
        payload={
            "outcome": "completed",
            "session_id": payload["session_id"],
            "result_text": payload.get("last_assistant_message"),
            "error": None,
            "error_details": None,
        },
    )
    return 0


def handle_stop_failure(payload: dict[str, Any]) -> int:
    store.append_message(
        direction="from_worker",
        message_type="run_finished",
        thread_id=_thread_id(),
        run_id=_run_id(),
        payload={
            "outcome": "api_error",
            "session_id": payload.get("session_id"),
            "result_text": payload.get("last_assistant_message"),
            "error": payload.get("error"),
            "error_details": payload.get("error_details"),
        },
    )
    return 0


def handle_notification(payload: dict[str, Any]) -> int:
    store.append_message(
        direction="from_worker",
        message_type="notification",
        thread_id=_thread_id(),
        run_id=_run_id(),
        payload={"text": payload.get("message", "")},
    )
    return 0


def _wait_for_permission_resolution(request_id: str) -> dict[str, Any] | None:
    timeout_seconds = float(os.environ.get("SINGLETON_PERMISSION_TIMEOUT", "300"))
    interval = float(os.environ.get("SINGLETON_PERMISSION_POLL_INTERVAL", "0.1"))
    deadline = time.monotonic() + timeout_seconds
    while time.monotonic() < deadline:
        with store.connect() as conn:
            row = conn.execute(
                """
                SELECT payload_json
                FROM messages
                WHERE message_type = 'permission_resolution'
                  AND thread_id = ?
                  AND run_id = ?
                  AND json_extract(payload_json, '$.request_id') = ?
                ORDER BY created_at DESC, rowid DESC
                LIMIT 1
                """,
                (_thread_id(), _run_id(), request_id),
            ).fetchone()
        if row is not None:
            return json.loads(row[0])
        time.sleep(interval)
    return None


def _configure_store_from_env() -> None:
    state_dir = Path(os.environ["SINGLETON_STATE_DIR"])
    db_path = Path(os.environ.get("SINGLETON_DB_PATH", state_dir / "messages.db"))
    store.configure_paths(state_dir=state_dir, db_path=db_path)
    store.init_dirs()


def _thread_id() -> str:
    return os.environ["SINGLETON_THREAD_ID"]


def _run_id() -> str:
    return os.environ["SINGLETON_RUN_ID"]


if __name__ == "__main__":
    raise SystemExit(main())
