"""Intentional failing placeholders for MCP integration work."""

import pytest


def test_t_mcp_1_create_thread_persists_durable_metadata():
    pytest.fail(
        "T-MCP-1 not implemented: create_thread should persist durable thread metadata"
    )


def test_t_mcp_2_send_to_thread_returns_run_finished_summary():
    pytest.fail(
        "T-MCP-2 not implemented: send_to_thread should wait for run_finished and return its summary"
    )


def test_t_mcp_3_thread_output_paginates_jsonl_logs():
    pytest.fail(
        "T-MCP-3 not implemented: thread_output should paginate JSONL-backed run output"
    )


def test_t_mcp_4_get_thread_events_newest_first():
    pytest.fail(
        "T-MCP-4 not implemented: get_thread_events should return durable lifecycle messages newest-first"
    )


def test_t_mcp_5_set_thread_permissions_updates_future_runs():
    pytest.fail(
        "T-MCP-5 not implemented: set_thread_permissions should update future-run mode"
    )


def test_t_mcp_6_list_pending_approvals_unresolved_only():
    pytest.fail(
        "T-MCP-6 not implemented: list_pending_approvals should return unresolved requests only"
    )


def test_t_mcp_7_deny_tool_call_persists_reason():
    pytest.fail(
        "T-MCP-7 not implemented: deny_tool_call should persist deny reason for hook consumption"
    )


def test_t_mcp_8_cancel_thread_cancels_active_run():
    pytest.fail(
        "T-MCP-8 not implemented: cancel_thread should cancel a thread's active run"
    )
