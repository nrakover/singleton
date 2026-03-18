"""Hook settings generation for worker processes."""

import json
from pathlib import Path


def generate_settings(thread_id: str, state_dir: Path, hooks_dir: Path) -> str:
    """Generate --settings JSON for worker spawn."""

    def hook_cmd(script: str) -> str:
        return (
            f"SINGLETON_THREAD_ID={thread_id} "
            f"SINGLETON_STATE_DIR={state_dir} "
            f"SINGLETON_HOOKS_DIR={hooks_dir} "
            f"{hooks_dir}/{script}"
        )

    settings = {
        "hooks": {
            "Stop": [
                {
                    "matcher": "*",
                    "hooks": [
                        {"type": "command", "command": hook_cmd("worker-stop.sh")}
                    ],
                }
            ],
            "PreToolUse": [
                {
                    "matcher": "*",
                    "hooks": [
                        {"type": "command", "command": hook_cmd("worker-pretool.sh")}
                    ],
                }
            ],
            "Notification": [
                {
                    "matcher": "*",
                    "hooks": [
                        {"type": "command", "command": hook_cmd("worker-notify.sh")}
                    ],
                }
            ],
        }
    }
    return json.dumps(settings)
