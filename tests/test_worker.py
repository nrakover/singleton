"""Intentional failing placeholders for worker session manager work."""

import pytest


def test_t_worker_1_create_run_before_spawn():
    pytest.fail("T-WORKER-1 not implemented: create run row before worker spawn")


def test_t_worker_2_direct_python_hooks():
    pytest.fail(
        "T-WORKER-2 not implemented: spawn command should inject direct Python hooks"
    )


def test_t_worker_3_inject_run_and_thread_env_vars():
    pytest.fail(
        "T-WORKER-3 not implemented: inject SINGLETON_THREAD_ID and SINGLETON_RUN_ID"
    )


def test_t_worker_4_resume_with_session_id():
    pytest.fail(
        "T-WORKER-4 not implemented: follow-up runs should use --resume <session_id>"
    )


def test_t_worker_5_yolo_uses_native_bypass_permissions():
    pytest.fail(
        "T-WORKER-5 not implemented: yolo mode should use Claude-native bypass permissions"
    )


def test_t_worker_6_stdout_teed_to_jsonl():
    pytest.fail(
        "T-WORKER-6 not implemented: stdout should be teed to {run_id}.stdout.jsonl"
    )


def test_t_worker_7_stderr_teed_to_jsonl():
    pytest.fail(
        "T-WORKER-7 not implemented: stderr should be teed to {run_id}.stderr.jsonl"
    )


def test_t_worker_8_reconcile_abnormal_exit():
    pytest.fail(
        "T-WORKER-8 not implemented: abnormal subprocess exit should be reconciled"
    )


def test_t_worker_9_update_thread_session_id_from_lifecycle():
    pytest.fail(
        "T-WORKER-9 not implemented: thread session_id should update from lifecycle data"
    )
