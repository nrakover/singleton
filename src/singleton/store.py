"""SQLite-backed durable state for singleton."""

from __future__ import annotations

import json
import secrets
import sqlite3
import threading
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

SINGLETON_DIR: Path = Path.home() / ".singleton"
THREADS_DIR: Path = SINGLETON_DIR / "threads"
DB_PATH: Path = SINGLETON_DIR / "messages.db"

_DB_LOCK = threading.Lock()


def _now_iso() -> str:
    now = datetime.now(timezone.utc)
    ms = now.microsecond // 1000
    return now.strftime(f"%Y-%m-%dT%H:%M:%S.{ms:03d}Z")


def _token(prefix: str) -> str:
    return f"{prefix}_{secrets.token_hex(6)}"


def generate_thread_id() -> str:
    return secrets.token_hex(3)


def generate_run_id() -> str:
    return _token("run")


def generate_message_id() -> str:
    return _token("msg")


def get_thread_dir(thread_id: str) -> Path:
    return THREADS_DIR / thread_id


def get_run_dir(thread_id: str) -> Path:
    return get_thread_dir(thread_id) / "runs"


def init_dirs() -> None:
    SINGLETON_DIR.mkdir(parents=True, exist_ok=True)
    THREADS_DIR.mkdir(parents=True, exist_ok=True)
    (SINGLETON_DIR / "workers" / "default").mkdir(parents=True, exist_ok=True)
    _initialize_db()


def configure_paths(state_dir: Path, db_path: Path | None = None) -> None:
    global SINGLETON_DIR, THREADS_DIR, DB_PATH
    SINGLETON_DIR = state_dir
    THREADS_DIR = SINGLETON_DIR / "threads"
    DB_PATH = db_path or (SINGLETON_DIR / "messages.db")


def connect() -> sqlite3.Connection:
    connection = sqlite3.connect(DB_PATH, check_same_thread=False)
    connection.row_factory = sqlite3.Row
    connection.execute("PRAGMA foreign_keys = ON")
    connection.execute("PRAGMA journal_mode = WAL")
    connection.execute("PRAGMA busy_timeout = 5000")
    return connection


def _initialize_db() -> None:
    with _DB_LOCK, connect() as conn:
        conn.executescript(
            """
            CREATE TABLE IF NOT EXISTS threads (
              thread_id TEXT PRIMARY KEY,
              description TEXT NOT NULL,
              context TEXT NOT NULL,
              cwd TEXT NOT NULL,
              permissions_mode TEXT NOT NULL,
              session_id TEXT,
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS runs (
              run_id TEXT PRIMARY KEY,
              thread_id TEXT NOT NULL REFERENCES threads(thread_id),
              created_at TEXT NOT NULL,
              pid INTEGER,
              finished_at TEXT,
              exit_code INTEGER
            );

            CREATE TABLE IF NOT EXISTS messages (
              message_id TEXT PRIMARY KEY,
              direction TEXT NOT NULL,
              message_type TEXT NOT NULL,
              thread_id TEXT NOT NULL REFERENCES threads(thread_id),
              run_id TEXT NOT NULL REFERENCES runs(run_id),
              payload_json TEXT NOT NULL,
              created_at TEXT NOT NULL
            );
            """
        )


def create_thread(
    description: str,
    context: str = "",
    cwd: str | None = None,
    permissions_mode: str = "supervised",
) -> dict[str, Any]:
    thread_id = generate_thread_id()
    if cwd is None:
        cwd = str(SINGLETON_DIR / "workers" / "default")
    now = _now_iso()
    get_run_dir(thread_id).mkdir(parents=True, exist_ok=True)
    with _DB_LOCK, connect() as conn:
        conn.execute(
            """
            INSERT INTO threads (
                thread_id, description, context, cwd, permissions_mode, session_id,
                created_at, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            """,
            (thread_id, description, context, cwd, permissions_mode, None, now, now),
        )
    return get_thread(thread_id)


def list_threads() -> list[dict[str, Any]]:
    with connect() as conn:
        rows = conn.execute("SELECT * FROM threads ORDER BY created_at DESC").fetchall()
    return [_row_to_dict(row) for row in rows]


def get_thread(thread_id: str) -> dict[str, Any]:
    with connect() as conn:
        row = conn.execute(
            "SELECT * FROM threads WHERE thread_id = ?", (thread_id,)
        ).fetchone()
    if row is None:
        raise FileNotFoundError(f"thread not found: {thread_id}")
    return _row_to_dict(row)


def update_thread(thread_id: str, **kwargs: Any) -> dict[str, Any]:
    if not kwargs:
        return get_thread(thread_id)
    allowed = {"description", "context", "cwd", "permissions_mode", "session_id"}
    unknown = set(kwargs) - allowed
    if unknown:
        raise ValueError(f"unknown thread fields: {sorted(unknown)}")
    fields = list(kwargs)
    assignments = ", ".join(f"{field} = ?" for field in fields)
    values = [kwargs[field] for field in fields]
    values.extend([_now_iso(), thread_id])
    with _DB_LOCK, connect() as conn:
        conn.execute(
            f"UPDATE threads SET {assignments}, updated_at = ? WHERE thread_id = ?",
            values,
        )
    return get_thread(thread_id)


def create_run(thread_id: str) -> dict[str, Any]:
    get_thread(thread_id)
    run_id = generate_run_id()
    now = _now_iso()
    get_run_dir(thread_id).mkdir(parents=True, exist_ok=True)
    with _DB_LOCK, connect() as conn:
        conn.execute(
            "INSERT INTO runs (run_id, thread_id, created_at, pid, finished_at, exit_code) VALUES (?, ?, ?, ?, ?, ?)",
            (run_id, thread_id, now, None, None, None),
        )
    return get_run(run_id)


def get_run(run_id: str) -> dict[str, Any]:
    with connect() as conn:
        row = conn.execute("SELECT * FROM runs WHERE run_id = ?", (run_id,)).fetchone()
    if row is None:
        raise FileNotFoundError(f"run not found: {run_id}")
    return _row_to_dict(row)


def update_run(run_id: str, **kwargs: Any) -> dict[str, Any]:
    if not kwargs:
        return get_run(run_id)
    allowed = {"pid", "finished_at", "exit_code"}
    unknown = set(kwargs) - allowed
    if unknown:
        raise ValueError(f"unknown run fields: {sorted(unknown)}")
    fields = list(kwargs)
    assignments = ", ".join(f"{field} = ?" for field in fields)
    values = [kwargs[field] for field in fields]
    values.append(run_id)
    with _DB_LOCK, connect() as conn:
        conn.execute(f"UPDATE runs SET {assignments} WHERE run_id = ?", values)
    return get_run(run_id)


def append_message(
    *,
    direction: str,
    message_type: str,
    thread_id: str,
    run_id: str,
    payload: dict[str, Any],
) -> dict[str, Any]:
    message_id = generate_message_id()
    created_at = _now_iso()
    payload_json = json.dumps(payload, sort_keys=True)
    with _DB_LOCK, connect() as conn:
        conn.execute(
            """
            INSERT INTO messages (
                message_id, direction, message_type, thread_id, run_id, payload_json, created_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?)
            """,
            (
                message_id,
                direction,
                message_type,
                thread_id,
                run_id,
                payload_json,
                created_at,
            ),
        )
    return {
        "message_id": message_id,
        "direction": direction,
        "message_type": message_type,
        "thread_id": thread_id,
        "run_id": run_id,
        "payload": payload,
        "created_at": created_at,
    }


def get_thread_events(
    thread_id: str, page: int = 0, page_size: int = 10
) -> dict[str, Any]:
    get_thread(thread_id)
    offset = page * page_size
    with connect() as conn:
        total = conn.execute(
            "SELECT COUNT(*) FROM messages WHERE thread_id = ?", (thread_id,)
        ).fetchone()[0]
        rows = conn.execute(
            """
            SELECT * FROM messages
            WHERE thread_id = ?
            ORDER BY created_at DESC, rowid DESC
            LIMIT ? OFFSET ?
            """,
            (thread_id, page_size, offset),
        ).fetchall()
    events = [_message_row_to_dict(row) for row in rows]
    return {
        "events": events,
        "total": total,
        "page": page,
        "has_more": offset + page_size < total,
    }


def list_pending_approvals() -> list[dict[str, Any]]:
    with connect() as conn:
        rows = conn.execute(
            """
            SELECT pr.*
            FROM messages AS pr
            WHERE pr.message_type = 'permission_request'
              AND NOT EXISTS (
                SELECT 1
                FROM messages AS rs
                WHERE rs.message_type = 'permission_resolution'
                  AND json_extract(rs.payload_json, '$.request_id') = json_extract(pr.payload_json, '$.request_id')
              )
            ORDER BY pr.created_at ASC, pr.rowid ASC
            """
        ).fetchall()
    return [_pending_from_row(row) for row in rows]


def thread_output(thread_id: str, page: int = 0, page_size: int = 50) -> dict[str, Any]:
    lines = _thread_output_lines(thread_id)
    total = len(lines)
    end = total - (page * page_size)
    start = max(0, end - page_size)
    end = max(0, end)
    return {
        "lines": lines[start:end],
        "total_lines": total,
        "page": page,
        "has_more": start > 0,
    }


def _thread_output_lines(thread_id: str) -> list[str]:
    run_dir = get_run_dir(thread_id)
    if not run_dir.exists():
        return []
    lines: list[str] = []
    for path in sorted(run_dir.glob("*.stdout.jsonl")):
        lines.extend(path.read_text().splitlines(keepends=True))
    return lines


def write_daemon_pid(pid: int) -> None:
    (SINGLETON_DIR / "daemon.pid").write_text(str(pid))


def read_daemon_pid() -> int | None:
    path = SINGLETON_DIR / "daemon.pid"
    if not path.exists():
        return None
    try:
        return int(path.read_text().strip())
    except ValueError:
        return None


def remove_daemon_pid() -> None:
    path = SINGLETON_DIR / "daemon.pid"
    if path.exists():
        path.unlink()


def write_mcp_port(port: int) -> None:
    (SINGLETON_DIR / "mcp.port").write_text(str(port))


def read_mcp_port() -> int | None:
    path = SINGLETON_DIR / "mcp.port"
    if not path.exists():
        return None
    try:
        return int(path.read_text().strip())
    except ValueError:
        return None


def _row_to_dict(row: sqlite3.Row) -> dict[str, Any]:
    return {key: row[key] for key in row.keys()}


def _message_row_to_dict(row: sqlite3.Row) -> dict[str, Any]:
    payload = json.loads(row["payload_json"])
    return {
        "message_id": row["message_id"],
        "direction": row["direction"],
        "message_type": row["message_type"],
        "thread_id": row["thread_id"],
        "run_id": row["run_id"],
        "payload": payload,
        "created_at": row["created_at"],
    }


def _pending_from_row(row: sqlite3.Row) -> dict[str, Any]:
    payload = json.loads(row["payload_json"])
    return {
        "request_id": payload["request_id"],
        "thread_id": row["thread_id"],
        "run_id": row["run_id"],
        "tool": payload["tool_name"],
        "input": payload["tool_input"],
        "created_at": row["created_at"],
    }
