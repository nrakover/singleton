"""Intentional failing placeholders for hub controller work."""

import pytest


def test_t_hub_1_launch_long_lived_streamed_session():
    pytest.fail(
        "T-HUB-1 not implemented: hub should launch as a daemon-owned long-lived streamed session"
    )


def test_t_hub_2_send_prompts_and_receive_streamed_output():
    pytest.fail(
        "T-HUB-2 not implemented: hub controller should send prompts and receive streamed output in memory"
    )


def test_t_hub_3_write_mcp_config_to_dot_mcp_json():
    pytest.fail(
        "T-HUB-3 not implemented: hub MCP config should be written to .mcp.json"
    )


def test_t_hub_4_preallow_singleton_tools():
    pytest.fail(
        "T-HUB-4 not implemented: hub settings should pre-allow singleton MCP tools"
    )
