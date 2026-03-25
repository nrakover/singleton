"""Intentional failing placeholders for daemon and recovery work."""

import pytest


def test_t_daemon_1_write_pid_and_bind_socket():
    pytest.fail(
        "T-DAEMON-1 not implemented: daemon should write daemon.pid and bind daemon.sock"
    )


def test_t_daemon_2_serve_mcp():
    pytest.fail(
        "T-DAEMON-2 not implemented: daemon should serve MCP on configured port"
    )


def test_t_daemon_3_rebuild_unresolved_permissions_on_restart():
    pytest.fail(
        "T-DAEMON-3 not implemented: restart should rebuild unresolved permission requests"
    )


def test_t_daemon_4_restart_without_stopping_workers():
    pytest.fail(
        "T-DAEMON-4 not implemented: daemon restart should not require worker shutdown"
    )


def test_t_daemon_5_worker_hooks_append_while_daemon_down():
    pytest.fail(
        "T-DAEMON-5 not implemented: hooks should continue appending while daemon is down"
    )


def test_t_daemon_6_surface_run_finished_from_hook_messages():
    pytest.fail(
        "T-DAEMON-6 not implemented: daemon should surface hook-authored run_finished messages"
    )


def test_t_daemon_7_approve_tool_call_appends_resolution():
    pytest.fail(
        "T-DAEMON-7 not implemented: approve_tool_call should append permission_resolution"
    )


def test_t_daemon_8_deny_tool_call_appends_resolution_with_reason():
    pytest.fail(
        "T-DAEMON-8 not implemented: deny_tool_call should append permission_resolution with reason"
    )


def test_t_daemon_9_rebuild_runtime_state_for_fresh_attaches():
    pytest.fail(
        "T-DAEMON-9 not implemented: daemon should rebuild canonical runtime state for new attaches"
    )
