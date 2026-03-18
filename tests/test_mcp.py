"""Tests for singleton.mcp_server module (T-MCP-1 through T-MCP-12)."""

from unittest.mock import AsyncMock, MagicMock

import pytest

import singleton.mcp_server as mcp_server
import singleton.store as store


@pytest.fixture(autouse=True)
def patch_store_dirs(tmp_path, monkeypatch):
    """Override store dirs to use tmp_path."""
    singleton_dir = tmp_path / ".singleton"
    threads_dir = singleton_dir / "threads"
    monkeypatch.setattr(store, "SINGLETON_DIR", singleton_dir)
    monkeypatch.setattr(store, "THREADS_DIR", threads_dir)
    store.init_dirs()
    return singleton_dir


@pytest.fixture(autouse=True)
def reset_worker_manager():
    """Reset worker manager before/after each test."""
    mcp_server.set_worker_manager(None)
    yield
    mcp_server.set_worker_manager(None)


# T-MCP-1: create_thread creates a thread with correct fields
async def test_create_thread():
    result = await mcp_server.create_thread(
        description="Test thread",
        context="some context",
        cwd="/tmp/repo",
        permissions_mode="supervised",
    )
    assert result["description"] == "Test thread"
    assert result["context"] == "some context"
    assert result["cwd"] == "/tmp/repo"
    assert result["permissions_mode"] == "supervised"
    assert "id" in result
    assert result["status"] == "pending"


# T-MCP-2: list_threads returns all threads
async def test_list_threads():
    await mcp_server.create_thread(description="Thread 1")
    await mcp_server.create_thread(description="Thread 2")
    threads = mcp_server.list_threads()
    assert len(threads) == 2


# T-MCP-3: get_thread returns thread with last_turn_summary
def test_get_thread():
    thread = store.create_thread(description="Get test")
    tid = thread["id"]
    store.append_output(tid, "some output\n")

    result = mcp_server.get_thread(thread_id=tid)
    assert result["id"] == tid
    assert "last_turn_summary" in result
    assert "some output" in result["last_turn_summary"]


# T-MCP-4: thread_output returns paginated output
def test_thread_output():
    thread = store.create_thread(description="Output test")
    tid = thread["id"]
    for i in range(10):
        store.append_output(tid, f"line {i}\n")

    result = mcp_server.thread_output(thread_id=tid, page=0, page_size=5)
    assert result["total_lines"] == 10
    assert len(result["lines"]) == 5
    assert result["has_more"] is True


# T-MCP-5: get_thread_events returns paginated events
def test_get_thread_events():
    thread = store.create_thread(description="Events test")
    tid = thread["id"]
    for i in range(3):
        store.write_event(tid, "stop", {"i": i})

    result = mcp_server.get_thread_events(thread_id=tid, page=0, page_size=2)
    assert result["total"] == 3
    assert len(result["events"]) == 2
    assert result["has_more"] is True


# T-MCP-6: send_to_thread calls worker manager
async def test_send_to_thread():
    thread = store.create_thread(description="Send test")
    tid = thread["id"]

    mock_manager = MagicMock()
    mock_manager.send = AsyncMock(return_value="result text")
    mcp_server.set_worker_manager(mock_manager)

    result = await mcp_server.send_to_thread(thread_id=tid, message="hello")
    assert result["thread_id"] == tid
    assert result["result"] == "result text"
    mock_manager.send.assert_called_once_with(tid, "hello")


# T-MCP-7: cancel_thread calls worker manager and updates status
async def test_cancel_thread():
    thread = store.create_thread(description="Cancel test")
    tid = thread["id"]

    mock_manager = MagicMock()
    mock_manager.cancel = AsyncMock()
    mcp_server.set_worker_manager(mock_manager)

    result = await mcp_server.cancel_thread(thread_id=tid)
    assert result["thread_id"] == tid
    assert result["status"] == "cancelled"
    mock_manager.cancel.assert_called_once_with(tid)


# T-MCP-8: set_thread_permissions updates permissions mode
def test_set_thread_permissions():
    thread = store.create_thread(description="Permissions test")
    tid = thread["id"]

    result = mcp_server.set_thread_permissions(thread_id=tid, mode="yolo")
    assert result["permissions_mode"] == "yolo"

    # Verify persisted
    t = store.get_thread(tid)
    assert t["permissions_mode"] == "yolo"


def test_set_thread_permissions_invalid():
    thread = store.create_thread(description="Invalid perm test")
    tid = thread["id"]

    with pytest.raises(ValueError, match="Invalid permissions mode"):
        mcp_server.set_thread_permissions(thread_id=tid, mode="invalid")


# T-MCP-9: list_pending_approvals returns pending requests
def test_list_pending_approvals():
    t1 = store.create_thread(description="Thread 1")
    t2 = store.create_thread(description="Thread 2")

    req1 = store.write_pending(t1["id"], "Bash", {"command": "ls"}, "supervised")
    store.write_pending(t2["id"], "Write", {"path": "/tmp/x"}, "supervised")
    # Approve req1
    store.write_response(req1["request_id"], t1["id"], "approve", "hub")

    result = mcp_server.list_pending_approvals()
    assert len(result) == 1
    assert result[0]["tool"] == "Write"


# T-MCP-10: approve_tool_call writes correct response
def test_approve_tool_call():
    thread = store.create_thread(description="Approve test")
    tid = thread["id"]

    req = store.write_pending(tid, "Bash", {"command": "ls"}, "supervised")
    req_id = req["request_id"]

    result = mcp_server.approve_tool_call(request_id=req_id)
    assert result["request_id"] == req_id
    assert result["decision"] == "approve"
    assert result["decided_by"] == "hub"

    # Verify stored
    resp = store.get_response(tid, req_id)
    assert resp is not None
    assert resp["decision"] == "approve"


# T-MCP-11: deny_tool_call writes correct response
def test_deny_tool_call():
    thread = store.create_thread(description="Deny test")
    tid = thread["id"]

    req = store.write_pending(tid, "Bash", {"command": "rm -rf /"}, "supervised")
    req_id = req["request_id"]

    result = mcp_server.deny_tool_call(request_id=req_id)
    assert result["request_id"] == req_id
    assert result["decision"] == "deny"

    resp = store.get_response(tid, req_id)
    assert resp is not None
    assert resp["decision"] == "deny"


# T-MCP-12: approve/deny on already-responded request raises error
def test_double_response_raises():
    thread = store.create_thread(description="Double response test")
    tid = thread["id"]

    req = store.write_pending(tid, "Bash", {"command": "ls"}, "supervised")
    req_id = req["request_id"]

    mcp_server.approve_tool_call(request_id=req_id)

    with pytest.raises(ValueError, match="already has a response"):
        mcp_server.approve_tool_call(request_id=req_id)


def test_approve_nonexistent_request():
    with pytest.raises(ValueError, match="not found"):
        mcp_server.approve_tool_call(request_id="nonexistent_id")


# T-MCP: send_to_thread without worker manager raises
async def test_send_to_thread_no_manager():
    thread = store.create_thread(description="No manager test")
    tid = thread["id"]

    with pytest.raises(RuntimeError, match="Worker manager not initialized"):
        await mcp_server.send_to_thread(thread_id=tid, message="hello")


# T-MCP: cancel_thread without worker manager raises
async def test_cancel_thread_no_manager():
    thread = store.create_thread(description="No manager cancel test")
    tid = thread["id"]

    with pytest.raises(RuntimeError, match="Worker manager not initialized"):
        await mcp_server.cancel_thread(thread_id=tid)
