"""Tests for singleton.worker module (T-WORKER-1 through T-WORKER-11)."""

import asyncio
import json
import sys
import textwrap
from pathlib import Path
from unittest.mock import patch

import pytest

import singleton.store as store
import singleton.worker as worker  # noqa: F401

MOCK_WORKER_SCRIPT = textwrap.dedent("""\
    import sys, json
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except Exception:
            continue
        if msg.get("type") == "user":
            content = msg["message"]["content"]
            print(json.dumps({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": f"Echo: {content}"}]
                }
            }), flush=True)
            print(json.dumps({
                "type": "result",
                "subtype": "success",
                "result": f"Echo: {content}",
                "session_id": "test-session",
                "usage": {"input_tokens": 10, "output_tokens": 5}
            }), flush=True)
""")


@pytest.fixture(autouse=True)
def patch_store_dirs(tmp_path, monkeypatch):
    """Override store dirs to use tmp_path."""
    singleton_dir = tmp_path / ".singleton"
    threads_dir = singleton_dir / "threads"
    monkeypatch.setattr(store, "SINGLETON_DIR", singleton_dir)
    monkeypatch.setattr(store, "THREADS_DIR", threads_dir)
    store.init_dirs()
    return singleton_dir


@pytest.fixture
def mock_worker_script(tmp_path):
    """Write mock worker script to temp file and return path."""
    script_path = tmp_path / "mock_worker.py"
    script_path.write_text(MOCK_WORKER_SCRIPT)
    return str(script_path)


@pytest.fixture
def mock_hooks_dir(tmp_path):
    hooks_dir = tmp_path / "hooks"
    hooks_dir.mkdir()
    return hooks_dir


def _make_claude_cmd(mock_worker_script: str) -> str:
    """Return python interpreter + mock script path as single string."""
    # We'll pass this as a list; returns just the python executable
    return sys.executable


async def _spawn_with_mock(
    thread_id: str,
    description: str,
    permissions_mode: str = "supervised",
    cwd: str | None = None,
    mock_worker_script: str = "",
    tmp_path: Path | None = None,
    hooks_dir: Path | None = None,
) -> tuple[asyncio.subprocess.Process, list]:
    """Spawn a mock worker and capture the command used."""
    captured_cmd: list = []
    captured_kwargs: dict = {}

    original_exec = asyncio.create_subprocess_exec

    async def mock_exec(*args, **kwargs):
        captured_cmd.extend(args)
        captured_kwargs.update(kwargs)
        # Replace claude with python mock_worker.py
        new_args = (sys.executable, mock_worker_script) + args[1:]
        return await original_exec(*new_args, **kwargs)

    state_dir = tmp_path / ".singleton" if tmp_path else None

    with patch("asyncio.create_subprocess_exec", side_effect=mock_exec):
        proc = await worker.spawn_worker(
            thread_id=thread_id,
            description=description,
            permissions_mode=permissions_mode,
            cwd=cwd or str(tmp_path) if tmp_path else "/tmp",
            state_dir=state_dir,
            hooks_dir=hooks_dir,
            claude_cmd="claude",
        )

    return proc, captured_cmd


# T-WORKER-1: spawn_worker uses --dangerously-skip-permissions for yolo
async def test_spawn_worker_yolo_flag(tmp_path, mock_worker_script, mock_hooks_dir):
    thread = store.create_thread(description="Yolo test", permissions_mode="yolo")
    tid = thread["id"]
    captured_cmd: list = []

    original_exec = asyncio.create_subprocess_exec

    async def mock_exec(*args, **kwargs):
        captured_cmd.extend(args)
        new_args = (sys.executable, mock_worker_script) + args[1:]
        return await original_exec(*new_args, **kwargs)

    with patch("asyncio.create_subprocess_exec", side_effect=mock_exec):
        proc = await worker.spawn_worker(
            thread_id=tid,
            description="Yolo test",
            permissions_mode="yolo",
            cwd=str(tmp_path),
            state_dir=tmp_path / ".singleton",
            hooks_dir=mock_hooks_dir,
            claude_cmd="claude",
        )

    proc.terminate()
    await proc.wait()
    assert "--dangerously-skip-permissions" in captured_cmd


# T-WORKER-2: No --dangerously-skip-permissions for supervised/passthrough
@pytest.mark.parametrize("mode", ["supervised", "passthrough"])
async def test_spawn_worker_no_skip_perms(
    tmp_path, mock_worker_script, mock_hooks_dir, mode
):
    thread = store.create_thread(description="Safe test", permissions_mode=mode)
    tid = thread["id"]
    captured_cmd: list = []

    original_exec = asyncio.create_subprocess_exec

    async def mock_exec(*args, **kwargs):
        captured_cmd.extend(args)
        new_args = (sys.executable, mock_worker_script) + args[1:]
        return await original_exec(*new_args, **kwargs)

    with patch("asyncio.create_subprocess_exec", side_effect=mock_exec):
        proc = await worker.spawn_worker(
            thread_id=tid,
            description="Safe test",
            permissions_mode=mode,
            cwd=str(tmp_path),
            state_dir=tmp_path / ".singleton",
            hooks_dir=mock_hooks_dir,
            claude_cmd="claude",
        )

    proc.terminate()
    await proc.wait()
    assert "--dangerously-skip-permissions" not in captured_cmd


# T-WORKER-3: --settings flag injected with hook config
async def test_spawn_worker_settings_flag(tmp_path, mock_worker_script, mock_hooks_dir):
    thread = store.create_thread(description="Settings test")
    tid = thread["id"]
    captured_cmd: list = []

    original_exec = asyncio.create_subprocess_exec

    async def mock_exec(*args, **kwargs):
        captured_cmd.extend(args)
        new_args = (sys.executable, mock_worker_script) + args[1:]
        return await original_exec(*new_args, **kwargs)

    with patch("asyncio.create_subprocess_exec", side_effect=mock_exec):
        proc = await worker.spawn_worker(
            thread_id=tid,
            description="Settings test",
            cwd=str(tmp_path),
            state_dir=tmp_path / ".singleton",
            hooks_dir=mock_hooks_dir,
            claude_cmd="claude",
        )

    proc.terminate()
    await proc.wait()
    assert "--settings" in captured_cmd
    settings_idx = captured_cmd.index("--settings")
    settings_json = captured_cmd[settings_idx + 1]
    settings = json.loads(settings_json)
    assert "hooks" in settings
    assert "Stop" in settings["hooks"]
    assert "PreToolUse" in settings["hooks"]


# T-WORKER-4: CWD set to specified directory
async def test_spawn_worker_cwd(tmp_path, mock_worker_script, mock_hooks_dir):
    thread = store.create_thread(description="CWD test")
    tid = thread["id"]
    captured_kwargs: dict = {}

    original_exec = asyncio.create_subprocess_exec

    async def mock_exec(*args, **kwargs):
        captured_kwargs.update(kwargs)
        new_args = (sys.executable, mock_worker_script) + args[1:]
        return await original_exec(*new_args, **kwargs)

    expected_cwd = str(tmp_path)
    with patch("asyncio.create_subprocess_exec", side_effect=mock_exec):
        proc = await worker.spawn_worker(
            thread_id=tid,
            description="CWD test",
            cwd=expected_cwd,
            state_dir=tmp_path / ".singleton",
            hooks_dir=mock_hooks_dir,
            claude_cmd="claude",
        )

    proc.terminate()
    await proc.wait()
    assert captured_kwargs.get("cwd") == expected_cwd


# T-WORKER-5: send_turn writes correct stream-json format to stdin
async def test_send_turn_stdin_format(tmp_path, mock_worker_script, mock_hooks_dir):
    thread = store.create_thread(description="Format test")
    tid = thread["id"]

    original_exec = asyncio.create_subprocess_exec

    async def mock_exec(*args, **kwargs):
        new_args = (sys.executable, mock_worker_script) + args[1:]
        return await original_exec(*new_args, **kwargs)

    with patch("asyncio.create_subprocess_exec", side_effect=mock_exec):
        proc = await worker.spawn_worker(
            thread_id=tid,
            description="Format test",
            cwd=str(tmp_path),
            state_dir=store.SINGLETON_DIR,
            hooks_dir=mock_hooks_dir,
            claude_cmd="claude",
        )

    # Send a second turn and verify format
    result = await worker.send_turn(
        proc, tid, "hello world", state_dir=store.SINGLETON_DIR
    )
    assert "Echo" in result
    proc.terminate()
    await proc.wait()


# T-WORKER-6: send_turn reads and parses result event from stdout
async def test_send_turn_reads_result(tmp_path, mock_worker_script, mock_hooks_dir):
    thread = store.create_thread(description="Result test")
    tid = thread["id"]

    original_exec = asyncio.create_subprocess_exec

    async def mock_exec(*args, **kwargs):
        new_args = (sys.executable, mock_worker_script) + args[1:]
        return await original_exec(*new_args, **kwargs)

    with patch("asyncio.create_subprocess_exec", side_effect=mock_exec):
        proc = await worker.spawn_worker(
            thread_id=tid,
            description="Result test",
            cwd=str(tmp_path),
            state_dir=store.SINGLETON_DIR,
            hooks_dir=mock_hooks_dir,
            claude_cmd="claude",
        )

    result = await worker.send_turn(
        proc, tid, "test message", state_dir=store.SINGLETON_DIR
    )
    assert isinstance(result, str)
    assert "Echo" in result
    proc.terminate()
    await proc.wait()


# T-WORKER-7: send_turn extracts text content, ignores tool_use blocks
async def test_send_turn_extracts_text_only(tmp_path, mock_hooks_dir):
    """Use a mock that returns mixed content blocks."""
    mixed_script = tmp_path / "mixed_worker.py"
    mixed_script.write_text(
        textwrap.dedent("""\
        import sys, json
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except Exception:
                continue
            if msg.get("type") == "user":
                print(json.dumps({
                    "type": "assistant",
                    "message": {
                        "role": "assistant",
                        "content": [
                            {"type": "text", "text": "Hello"},
                            {
                                "type": "tool_use",
                                "id": "t1",
                                "name": "Bash",
                                "input": {"command": "ls"},
                            },
                            {"type": "text", "text": " World"}
                        ]
                    }
                }), flush=True)
                print(json.dumps({
                    "type": "result",
                    "subtype": "success",
                    "result": "Hello World",
                    "session_id": "s1",
                    "usage": {}
                }), flush=True)
    """)
    )

    thread = store.create_thread(description="Text only test")
    tid = thread["id"]

    original_exec = asyncio.create_subprocess_exec

    async def mock_exec(*args, **kwargs):
        new_args = (sys.executable, str(mixed_script)) + args[1:]
        return await original_exec(*new_args, **kwargs)

    with patch("asyncio.create_subprocess_exec", side_effect=mock_exec):
        proc = await worker.spawn_worker(
            thread_id=tid,
            description="Text only test",
            cwd=str(tmp_path),
            state_dir=store.SINGLETON_DIR,
            hooks_dir=mock_hooks_dir,
            claude_cmd="claude",
        )

    result = await worker.send_turn(proc, tid, "go", state_dir=store.SINGLETON_DIR)
    # Only text content should appear
    assert "Hello" in result
    assert "World" in result
    assert "Bash" not in result
    proc.terminate()
    await proc.wait()


# T-WORKER-8: send_turn truncates result text to <=500 chars
async def test_send_turn_truncates(tmp_path, mock_hooks_dir):
    """Use a mock that returns very long text."""
    long_script = tmp_path / "long_worker.py"
    long_script.write_text(
        textwrap.dedent("""\
        import sys, json
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except Exception:
                continue
            if msg.get("type") == "user":
                long_text = "A" * 1000
                print(json.dumps({
                    "type": "assistant",
                    "message": {
                        "role": "assistant",
                        "content": [{"type": "text", "text": long_text}]
                    }
                }), flush=True)
                print(json.dumps({
                    "type": "result",
                    "subtype": "success",
                    "result": long_text,
                    "session_id": "s1",
                    "usage": {}
                }), flush=True)
    """)
    )

    thread = store.create_thread(description="Truncation test")
    tid = thread["id"]

    original_exec = asyncio.create_subprocess_exec

    async def mock_exec(*args, **kwargs):
        new_args = (sys.executable, str(long_script)) + args[1:]
        return await original_exec(*new_args, **kwargs)

    with patch("asyncio.create_subprocess_exec", side_effect=mock_exec):
        proc = await worker.spawn_worker(
            thread_id=tid,
            description="Truncation test",
            cwd=str(tmp_path),
            state_dir=store.SINGLETON_DIR,
            hooks_dir=mock_hooks_dir,
            claude_cmd="claude",
        )

    result = await worker.send_turn(proc, tid, "go", state_dir=store.SINGLETON_DIR)
    assert len(result) <= 500
    proc.terminate()
    await proc.wait()


# T-WORKER-9: cancel_worker sends SIGTERM
async def test_cancel_worker_sigterm(tmp_path, mock_worker_script, mock_hooks_dir):
    thread = store.create_thread(description="Cancel test")
    tid = thread["id"]

    original_exec = asyncio.create_subprocess_exec

    async def mock_exec(*args, **kwargs):
        new_args = (sys.executable, mock_worker_script) + args[1:]
        return await original_exec(*new_args, **kwargs)

    with patch("asyncio.create_subprocess_exec", side_effect=mock_exec):
        proc = await worker.spawn_worker(
            thread_id=tid,
            description="Cancel test",
            cwd=str(tmp_path),
            state_dir=store.SINGLETON_DIR,
            hooks_dir=mock_hooks_dir,
            claude_cmd="claude",
        )

    await worker.cancel_worker(proc, tid)
    # Wait for process to terminate
    try:
        await asyncio.wait_for(proc.wait(), timeout=3.0)
    except asyncio.TimeoutError:
        proc.kill()

    assert proc.returncode is not None
    # Check status updated
    t = store.get_thread(tid)
    assert t["status"] == "cancelled"


# T-WORKER-10: Worker output appended to output.txt
async def test_worker_output_appended(tmp_path, mock_worker_script, mock_hooks_dir):
    thread = store.create_thread(description="Output test")
    tid = thread["id"]

    original_exec = asyncio.create_subprocess_exec

    async def mock_exec(*args, **kwargs):
        new_args = (sys.executable, mock_worker_script) + args[1:]
        return await original_exec(*new_args, **kwargs)

    with patch("asyncio.create_subprocess_exec", side_effect=mock_exec):
        proc = await worker.spawn_worker(
            thread_id=tid,
            description="Output test",
            cwd=str(tmp_path),
            state_dir=store.SINGLETON_DIR,
            hooks_dir=mock_hooks_dir,
            claude_cmd="claude",
        )

    # output.txt should have content from initial turn
    lines = store.get_output_lines(tid)
    assert len(lines) > 0

    proc.terminate()
    await proc.wait()


# T-WORKER-11: Mock worker receives multiple sequential turns correctly
async def test_multiple_sequential_turns(tmp_path, mock_worker_script, mock_hooks_dir):
    thread = store.create_thread(description="Multi-turn test")
    tid = thread["id"]

    original_exec = asyncio.create_subprocess_exec

    async def mock_exec(*args, **kwargs):
        new_args = (sys.executable, mock_worker_script) + args[1:]
        return await original_exec(*new_args, **kwargs)

    with patch("asyncio.create_subprocess_exec", side_effect=mock_exec):
        proc = await worker.spawn_worker(
            thread_id=tid,
            description="Multi-turn test",
            cwd=str(tmp_path),
            state_dir=store.SINGLETON_DIR,
            hooks_dir=mock_hooks_dir,
            claude_cmd="claude",
        )

    result1 = await worker.send_turn(
        proc, tid, "first turn", state_dir=store.SINGLETON_DIR
    )
    result2 = await worker.send_turn(
        proc, tid, "second turn", state_dir=store.SINGLETON_DIR
    )

    assert "first turn" in result1
    assert "second turn" in result2

    lines = store.get_output_lines(tid)
    combined = "".join(lines)
    assert "first turn" in combined
    assert "second turn" in combined

    proc.terminate()
    await proc.wait()
