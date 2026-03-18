"""State management for singleton threads."""

import fcntl
import json
import secrets
import threading
from datetime import datetime, timezone
from pathlib import Path

SINGLETON_DIR: Path = Path.home() / ".singleton"
THREADS_DIR: Path = SINGLETON_DIR / "threads"

# Per-thread file locks for append_output
_output_locks: dict[str, threading.Lock] = {}
_output_locks_mutex = threading.Lock()


def _get_output_lock(thread_id: str) -> threading.Lock:
    with _output_locks_mutex:
        if thread_id not in _output_locks:
            _output_locks[thread_id] = threading.Lock()
        return _output_locks[thread_id]


def _now_iso() -> str:
    now = datetime.now(timezone.utc)
    ms = now.microsecond // 1000
    return now.strftime(f"%Y-%m-%dT%H:%M:%S.{ms:03d}Z")


def _ms_now() -> int:
    import time

    return int(time.time() * 1000)


def generate_thread_id() -> str:
    """Generate a short unique thread ID (6 hex chars)."""
    return secrets.token_hex(3)


def generate_event_id(event_type: str) -> str:
    """Generate event_id in format {unix_ms}-{type}-{random4}."""
    ms = _ms_now()
    rand = secrets.token_hex(2)
    return f"{ms}-{event_type}-{rand}"


def generate_request_id() -> str:
    """Generate request_id in format req_{unix_ms}_{random4}."""
    ms = _ms_now()
    rand = secrets.token_hex(2)
    return f"req_{ms}_{rand}"


def get_thread_dir(thread_id: str) -> Path:
    return THREADS_DIR / thread_id


def init_dirs() -> None:
    """Create ~/.singleton/ directory structure."""
    SINGLETON_DIR.mkdir(parents=True, exist_ok=True)
    (SINGLETON_DIR / "workers" / "default").mkdir(parents=True, exist_ok=True)
    THREADS_DIR.mkdir(parents=True, exist_ok=True)


def create_thread(
    description: str,
    context: str = "",
    cwd: str | None = None,
    permissions_mode: str = "supervised",
) -> dict:
    """Create thread.json, events/, pending/, responses/ dirs. Returns thread dict."""
    thread_id = generate_thread_id()
    thread_dir = get_thread_dir(thread_id)
    thread_dir.mkdir(parents=True, exist_ok=True)
    (thread_dir / "events").mkdir(exist_ok=True)
    (thread_dir / "pending").mkdir(exist_ok=True)
    (thread_dir / "responses").mkdir(exist_ok=True)

    if cwd is None:
        cwd = str(SINGLETON_DIR / "workers" / "default")

    now = _now_iso()
    thread = {
        "id": thread_id,
        "description": description,
        "context": context,
        "cwd": cwd,
        "status": "pending",
        "permissions_mode": permissions_mode,
        "pid": None,
        "session_id": None,
        "created_at": now,
        "updated_at": now,
    }
    _write_thread_json(thread_id, thread)
    return thread


def _write_thread_json(thread_id: str, data: dict) -> None:
    thread_dir = get_thread_dir(thread_id)
    path = thread_dir / "thread.json"
    path.write_text(json.dumps(data, indent=2))


def get_thread(thread_id: str) -> dict:
    """Read thread.json. Raises FileNotFoundError if not found."""
    path = get_thread_dir(thread_id) / "thread.json"
    if not path.exists():
        raise FileNotFoundError(f"Thread {thread_id} not found")
    data = json.loads(path.read_text())
    # Compute last_turn_summary from output.txt
    lines = get_output_lines(thread_id)
    if lines:
        last_turn = "".join(lines)[-500:]
    else:
        last_turn = ""
    data["last_turn_summary"] = last_turn
    return data


def list_threads() -> list[dict]:
    """List all threads sorted by created_at descending."""
    threads = []
    if not THREADS_DIR.exists():
        return threads
    for thread_dir in THREADS_DIR.iterdir():
        if thread_dir.is_dir():
            try:
                t = get_thread(thread_dir.name)
                threads.append(t)
            except (FileNotFoundError, json.JSONDecodeError):
                continue
    threads.sort(key=lambda t: t["created_at"], reverse=True)
    return threads


def update_thread(thread_id: str, **kwargs) -> dict:
    """Update thread.json fields. Always updates updated_at."""
    thread = get_thread(thread_id)
    # Remove computed field before writing
    thread.pop("last_turn_summary", None)
    thread.update(kwargs)
    thread["updated_at"] = _now_iso()
    _write_thread_json(thread_id, thread)
    return get_thread(thread_id)


def update_thread_status(thread_id: str, status: str) -> dict:
    """Convenience: update status field."""
    return update_thread(thread_id, status=status)


def append_output(thread_id: str, text: str) -> None:
    """Append text to output.txt (thread-safe)."""
    path = get_thread_dir(thread_id) / "output.txt"
    lock = _get_output_lock(thread_id)
    with lock:
        with open(path, "a") as f:
            fcntl.flock(f, fcntl.LOCK_EX)
            try:
                f.write(text)
            finally:
                fcntl.flock(f, fcntl.LOCK_UN)


def get_output_lines(thread_id: str) -> list[str]:
    """Read all lines from output.txt. Returns [] if file doesn't exist."""
    path = get_thread_dir(thread_id) / "output.txt"
    if not path.exists():
        return []
    return path.read_text().splitlines(keepends=True)


def thread_output(thread_id: str, page: int = 0, page_size: int = 50) -> dict:
    """Paginated output. page=0=latest. Returns {lines, total_lines, page, has_more}."""
    all_lines = get_output_lines(thread_id)
    total = len(all_lines)

    # page=0 → last page_size lines; page=1 → prior page_size lines; etc.
    end = total - (page * page_size)
    start = end - page_size

    has_more = start > 0
    start = max(0, start)
    end = max(0, end)

    lines = all_lines[start:end]
    return {
        "lines": lines,
        "total_lines": total,
        "page": page,
        "has_more": has_more,
    }


def write_event(thread_id: str, event_type: str, data: dict) -> dict:
    """Write event file. Returns event dict."""
    event_id = generate_event_id(event_type)
    event = {
        "event_id": event_id,
        "thread_id": thread_id,
        "type": event_type,
        "data": data,
        "timestamp": _now_iso(),
    }
    path = get_thread_dir(thread_id) / "events" / f"{event_id}.json"
    path.write_text(json.dumps(event, indent=2))
    return event


def _load_event(path: Path) -> dict:
    return json.loads(path.read_text())


def get_thread_events(thread_id: str, page: int = 0, page_size: int = 10) -> dict:
    """Paginated events sorted newest-first. Returns {events, total, page, has_more}."""
    events_dir = get_thread_dir(thread_id) / "events"
    if not events_dir.exists():
        return {"events": [], "total": 0, "page": page, "has_more": False}

    files = sorted(events_dir.glob("*.json"), key=lambda p: p.name)
    # Newest first
    files = list(reversed(files))
    total = len(files)

    start = page * page_size
    end = start + page_size
    page_files = files[start:end]
    has_more = end < total

    events = [_load_event(f) for f in page_files]
    return {
        "events": events,
        "total": total,
        "page": page,
        "has_more": has_more,
    }


def write_pending(thread_id: str, tool: str, input: dict, mode: str) -> dict:
    """Write pending approval request. Returns request dict."""
    req_id = generate_request_id()
    req = {
        "request_id": req_id,
        "thread_id": thread_id,
        "tool": tool,
        "input": input,
        "mode": mode,
        "created_at": _now_iso(),
    }
    path = get_thread_dir(thread_id) / "pending" / f"{req_id}.json"
    path.write_text(json.dumps(req, indent=2))
    return req


def write_response(
    request_id: str, thread_id: str, decision: str, decided_by: str
) -> dict:
    """Write approval response. Returns response dict."""
    resp = {
        "request_id": request_id,
        "decision": decision,
        "decided_by": decided_by,
        "decided_at": _now_iso(),
    }
    path = get_thread_dir(thread_id) / "responses" / f"{request_id}.json"
    path.write_text(json.dumps(resp, indent=2))
    return resp


def get_pending_request(thread_id: str, request_id: str) -> dict | None:
    """Read pending/{request_id}.json. Returns None if not found."""
    path = get_thread_dir(thread_id) / "pending" / f"{request_id}.json"
    if not path.exists():
        return None
    return json.loads(path.read_text())


def get_response(thread_id: str, request_id: str) -> dict | None:
    """Read responses/{request_id}.json. Returns None if not found."""
    path = get_thread_dir(thread_id) / "responses" / f"{request_id}.json"
    if not path.exists():
        return None
    return json.loads(path.read_text())


def list_pending_approvals() -> list[dict]:
    """Return all pending requests with no response file, sorted ascending."""
    result = []
    if not THREADS_DIR.exists():
        return result

    for thread_dir in THREADS_DIR.iterdir():
        if not thread_dir.is_dir():
            continue
        pending_dir = thread_dir / "pending"
        if not pending_dir.exists():
            continue
        for req_file in pending_dir.glob("*.json"):
            req = json.loads(req_file.read_text())
            req_id = req["request_id"]
            thread_id = req["thread_id"]
            resp = get_response(thread_id, req_id)
            if resp is None:
                result.append(req)

    result.sort(key=lambda r: r["created_at"])
    return result


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


def write_hub_session_id(session_id: str) -> None:
    (SINGLETON_DIR / "hub_session_id").write_text(session_id)


def read_hub_session_id() -> str | None:
    path = SINGLETON_DIR / "hub_session_id"
    if not path.exists():
        return None
    return path.read_text().strip()


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
