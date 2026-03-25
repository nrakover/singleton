"""Intentional failing placeholders for TUI multi-attach work."""

import pytest


def test_t_tui_1_multiple_clients_receive_mirrored_state():
    pytest.fail(
        "T-TUI-1 not implemented: multiple clients should receive mirrored rendered state"
    )


def test_t_tui_2_exactly_one_client_owns_hub_input():
    pytest.fail(
        "T-TUI-2 not implemented: exactly one client should own freeform hub input"
    )


def test_t_tui_3_non_owners_observe_but_cannot_type():
    pytest.fail(
        "T-TUI-3 not implemented: non-owning clients should observe but not type into the hub"
    )


def test_t_tui_4_control_handoff_updates_ownership():
    pytest.fail(
        "T-TUI-4 not implemented: control handoff should update ownership cleanly"
    )


def test_t_tui_5_passthrough_routes_to_active_owner():
    pytest.fail(
        "T-TUI-5 not implemented: passthrough approval should route to the active input owner"
    )


def test_t_tui_6_detach_removes_only_that_client():
    pytest.fail("T-TUI-6 not implemented: detach should remove only that client")
