"""Tests for singleton.store module (T-STORE-1 through T-STORE-13)."""

import json
import threading
import time

import pytest

import singleton.store as store


@pytest.fixture(autouse=True)
def patch_store_dirs(tmp_path, monkeypatch):
    """Override SINGLETON_DIR and THREADS_DIR to use tmp_path."""
    singleton_dir = tmp_path / ".singleton"
    threads_dir = singleton_dir / "threads"
    monkeypatch.setattr(store, "SINGLETON_DIR", singleton_dir)
    monkeypatch.setattr(store, "THREADS_DIR", threads_dir)
    store.init_dirs()
    return singleton_dir


# T-STORE-1: create_thread creates thread.json with correct fields
def test_create_thread_correct_fields():
    thread = store.create_thread(
        description="Test task",
        context="some context",
        cwd="/tmp/myrepo",
        permissions_mode="yolo",
    )
    assert thread["description"] == "Test task"
    assert thread["context"] == "some context"
    assert thread["cwd"] == "/tmp/myrepo"
    assert thread["permissions_mode"] == "yolo"
    assert thread["status"] == "pending"
    assert thread["pid"] is None
    assert thread["session_id"] is None
    assert "id" in thread
    assert "created_at" in thread
    assert "updated_at" in thread

    # Verify file exists and is valid JSON
    thread_dir = store.get_thread_dir(thread["id"])
    thread_json = thread_dir / "thread.json"
    assert thread_json.exists()
    loaded = json.loads(thread_json.read_text())
    assert loaded["id"] == thread["id"]


# T-STORE-2: create_thread with cwd=None sets cwd to ~/.singleton/workers/default/
def test_create_thread_default_cwd():
    thread = store.create_thread(description="No cwd")
    expected_cwd = str(store.SINGLETON_DIR / "workers" / "default")
    assert thread["cwd"] == expected_cwd


# T-STORE-3: list_threads returns all threads sorted by created_at descending
def test_list_threads_sorted():
    t1 = store.create_thread(description="First")
    time.sleep(0.01)
    t2 = store.create_thread(description="Second")
    time.sleep(0.01)
    t3 = store.create_thread(description="Third")

    threads = store.list_threads()
    assert len(threads) == 3
    # Newest first
    assert threads[0]["id"] == t3["id"]
    assert threads[1]["id"] == t2["id"]
    assert threads[2]["id"] == t1["id"]


# T-STORE-4: get_thread returns full metadata including last_turn_summary
def test_get_thread_with_last_turn_summary():
    thread = store.create_thread(description="Output test")
    tid = thread["id"]

    # No output yet
    t = store.get_thread(tid)
    assert t["last_turn_summary"] == ""

    # Write output
    store.append_output(tid, "line one\n")
    store.append_output(tid, "line two\n")

    t = store.get_thread(tid)
    assert "line one" in t["last_turn_summary"]
    assert "line two" in t["last_turn_summary"]


def test_get_thread_not_found():
    with pytest.raises(FileNotFoundError):
        store.get_thread("nonexistent")


# T-STORE-5: update_thread_status transitions status and updates updated_at
def test_update_thread_status():
    thread = store.create_thread(description="Status test")
    tid = thread["id"]
    original_updated_at = thread["updated_at"]

    time.sleep(0.01)
    updated = store.update_thread_status(tid, "running")
    assert updated["status"] == "running"
    assert updated["updated_at"] != original_updated_at


# T-STORE-6: Writing an event file creates correct schema
def test_write_event_schema():
    thread = store.create_thread(description="Event test")
    tid = thread["id"]

    event = store.write_event(tid, "stop", {"session_id": "sess123"})
    assert event["thread_id"] == tid
    assert event["type"] == "stop"
    assert event["data"]["session_id"] == "sess123"
    assert "event_id" in event
    assert "timestamp" in event

    # Check file exists
    events_dir = store.get_thread_dir(tid) / "events"
    files = list(events_dir.glob("*.json"))
    assert len(files) == 1
    loaded = json.loads(files[0].read_text())
    assert loaded["event_id"] == event["event_id"]


# T-STORE-7: Writing a pending approval creates correct schema
def test_write_pending_schema():
    thread = store.create_thread(description="Pending test")
    tid = thread["id"]

    req = store.write_pending(tid, "Bash", {"command": "rm -rf /tmp/foo"}, "supervised")
    assert req["thread_id"] == tid
    assert req["tool"] == "Bash"
    assert req["input"]["command"] == "rm -rf /tmp/foo"
    assert req["mode"] == "supervised"
    assert "request_id" in req
    assert "created_at" in req

    path = store.get_thread_dir(tid) / "pending" / f"{req['request_id']}.json"
    assert path.exists()
    loaded = json.loads(path.read_text())
    assert loaded["request_id"] == req["request_id"]


# T-STORE-8: Writing a response creates correct schema
def test_write_response_schema():
    thread = store.create_thread(description="Response test")
    tid = thread["id"]

    req = store.write_pending(tid, "Bash", {"command": "ls"}, "supervised")
    resp = store.write_response(req["request_id"], tid, "approve", "hub")

    assert resp["request_id"] == req["request_id"]
    assert resp["decision"] == "approve"
    assert resp["decided_by"] == "hub"
    assert "decided_at" in resp

    loaded = store.get_response(tid, req["request_id"])
    assert loaded is not None
    assert loaded["decision"] == "approve"


# T-STORE-9: thread_output pagination: page=0=last N; page=1=prior N; has_more correct
def test_thread_output_pagination():
    thread = store.create_thread(description="Pagination test")
    tid = thread["id"]

    # Write 15 lines
    for i in range(15):
        store.append_output(tid, f"line {i}\n")

    # page=0, page_size=5 → last 5 lines (10-14)
    result = store.thread_output(tid, page=0, page_size=5)
    assert result["total_lines"] == 15
    assert result["page"] == 0
    assert len(result["lines"]) == 5
    assert "line 14" in result["lines"][-1]
    assert "line 10" in result["lines"][0]
    assert result["has_more"] is True

    # page=1, page_size=5 → lines 5-9
    result = store.thread_output(tid, page=1, page_size=5)
    assert len(result["lines"]) == 5
    assert "line 5" in result["lines"][0]
    assert "line 9" in result["lines"][-1]
    assert result["has_more"] is True

    # page=2, page_size=5 → lines 0-4
    result = store.thread_output(tid, page=2, page_size=5)
    assert len(result["lines"]) == 5
    assert "line 0" in result["lines"][0]
    assert result["has_more"] is False


# T-STORE-10: thread_output with fewer lines than page_size returns all, has_more=False
def test_thread_output_fewer_lines():
    thread = store.create_thread(description="Few lines test")
    tid = thread["id"]

    store.append_output(tid, "only line\n")
    result = store.thread_output(tid, page=0, page_size=50)
    assert len(result["lines"]) == 1
    assert result["has_more"] is False
    assert result["total_lines"] == 1


# T-STORE-11: get_thread_events pagination: page=0=latest N; incrementing walks back
def test_get_thread_events_pagination():
    thread = store.create_thread(description="Events pagination")
    tid = thread["id"]

    event_ids = []
    for i in range(5):
        e = store.write_event(tid, "stop", {"i": i})
        event_ids.append(e["event_id"])
        time.sleep(0.001)  # ensure different timestamps

    # page=0, page_size=2 → 2 newest
    result = store.get_thread_events(tid, page=0, page_size=2)
    assert result["total"] == 5
    assert len(result["events"]) == 2
    assert result["has_more"] is True
    # newest first
    assert result["events"][0]["event_id"] == event_ids[4]
    assert result["events"][1]["event_id"] == event_ids[3]

    # page=1 → next 2
    result = store.get_thread_events(tid, page=1, page_size=2)
    assert len(result["events"]) == 2
    assert result["events"][0]["event_id"] == event_ids[2]
    assert result["events"][1]["event_id"] == event_ids[1]
    assert result["has_more"] is True

    # page=2 → oldest 1
    result = store.get_thread_events(tid, page=2, page_size=2)
    assert len(result["events"]) == 1
    assert result["events"][0]["event_id"] == event_ids[0]
    assert result["has_more"] is False


# T-STORE-12: Concurrent writes to output.txt don't corrupt file
def test_concurrent_output_writes():
    thread = store.create_thread(description="Concurrent test")
    tid = thread["id"]

    num_threads = 10
    lines_per_thread = 50

    def write_lines(prefix: str):
        for i in range(lines_per_thread):
            store.append_output(tid, f"{prefix}-{i}\n")

    threads = [
        threading.Thread(target=write_lines, args=(f"t{j}",))
        for j in range(num_threads)
    ]
    for t in threads:
        t.start()
    for t in threads:
        t.join()

    lines = store.get_output_lines(tid)
    assert len(lines) == num_threads * lines_per_thread
    # Each line should be complete (no partial writes)
    for line in lines:
        assert line.endswith("\n")


# T-STORE-13: thread.json is valid JSON after any store operation
def test_thread_json_valid_after_operations():
    thread = store.create_thread(description="JSON validity test")
    tid = thread["id"]

    store.update_thread_status(tid, "running")
    store.update_thread(tid, pid=12345, session_id="sess-abc")

    path = store.get_thread_dir(tid) / "thread.json"
    content = path.read_text()
    loaded = json.loads(content)  # Should not raise
    assert loaded["status"] == "running"
    assert loaded["pid"] == 12345
    assert loaded["session_id"] == "sess-abc"


# Additional: list_pending_approvals
def test_list_pending_approvals():
    t1 = store.create_thread(description="Thread 1")
    t2 = store.create_thread(description="Thread 2")

    req1 = store.write_pending(t1["id"], "Bash", {"command": "ls"}, "supervised")
    req2 = store.write_pending(t2["id"], "Write", {"path": "/tmp/x"}, "supervised")
    # Approve req1
    store.write_response(req1["request_id"], t1["id"], "approve", "hub")

    pending = store.list_pending_approvals()
    assert len(pending) == 1
    assert pending[0]["request_id"] == req2["request_id"]
