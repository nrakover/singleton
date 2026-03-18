"""MCP server for singleton daemon."""

import asyncio

from fastmcp import FastMCP

from singleton import store

mcp = FastMCP("singleton")

# Set by daemon before starting server
_worker_manager = None


def set_worker_manager(manager) -> None:
    global _worker_manager
    _worker_manager = manager


@mcp.tool()
async def create_thread(
    description: str,
    context: str = "",
    cwd: str | None = None,
    permissions_mode: str = "supervised",
) -> dict:
    """Create a new background worker thread."""
    thread = store.create_thread(
        description=description,
        context=context,
        cwd=cwd,
        permissions_mode=permissions_mode,
    )
    if _worker_manager is not None:
        asyncio.create_task(_worker_manager.spawn(thread["id"]))
    return thread


@mcp.tool()
def list_threads() -> list:
    """List all threads sorted by created_at descending."""
    return store.list_threads()


@mcp.tool()
def get_thread(thread_id: str) -> dict:
    """Get thread metadata including last_turn_summary."""
    return store.get_thread(thread_id)


@mcp.tool()
def thread_output(thread_id: str, page: int = 0, page_size: int = 50) -> dict:
    """Get paginated output for a thread. page=0=latest."""
    return store.thread_output(thread_id, page=page, page_size=page_size)


@mcp.tool()
def get_thread_events(thread_id: str, page: int = 0, page_size: int = 10) -> dict:
    """Get paginated events for a thread. page=0=newest."""
    return store.get_thread_events(thread_id, page=page, page_size=page_size)


@mcp.tool()
async def send_to_thread(thread_id: str, message: str) -> dict:
    """Send a message to a running thread and get the result."""
    if _worker_manager is None:
        raise RuntimeError("Worker manager not initialized")
    result_text = await _worker_manager.send(thread_id, message)
    return {"thread_id": thread_id, "result": result_text}


@mcp.tool()
async def cancel_thread(thread_id: str) -> dict:
    """Cancel a running thread."""
    if _worker_manager is None:
        raise RuntimeError("Worker manager not initialized")
    await _worker_manager.cancel(thread_id)
    thread = store.update_thread_status(thread_id, "cancelled")
    return {"thread_id": thread_id, "status": thread["status"]}


@mcp.tool()
def set_thread_permissions(thread_id: str, mode: str) -> dict:
    """Set permissions mode for a thread (yolo/supervised/passthrough)."""
    valid_modes = {"yolo", "supervised", "passthrough"}
    if mode not in valid_modes:
        raise ValueError(
            f"Invalid permissions mode: {mode}. Must be one of {valid_modes}"
        )
    thread = store.update_thread(thread_id, permissions_mode=mode)
    return {"thread_id": thread_id, "permissions_mode": thread["permissions_mode"]}


@mcp.tool()
def list_pending_approvals() -> list:
    """List all pending tool approval requests across all threads."""
    return store.list_pending_approvals()


@mcp.tool()
def approve_tool_call(request_id: str) -> dict:
    """Approve a pending tool call."""
    return _resolve_tool_call(request_id, "approve")


@mcp.tool()
def deny_tool_call(request_id: str) -> dict:
    """Deny a pending tool call."""
    return _resolve_tool_call(request_id, "deny")


def _resolve_tool_call(request_id: str, decision: str) -> dict:
    """Internal: find the request across all threads and write a response."""
    # Find request across all threads
    thread_id = None
    if store.THREADS_DIR.exists():
        for thread_dir in store.THREADS_DIR.iterdir():
            if not thread_dir.is_dir():
                continue
            req = store.get_pending_request(thread_dir.name, request_id)
            if req is not None:
                thread_id = thread_dir.name
                break

    if thread_id is None:
        raise ValueError(f"Request {request_id} not found")

    # Check no existing response
    existing = store.get_response(thread_id, request_id)
    if existing is not None:
        raise ValueError(
            f"Request {request_id} already has a response: {existing['decision']}"
        )

    resp = store.write_response(request_id, thread_id, decision, "hub")
    return resp
