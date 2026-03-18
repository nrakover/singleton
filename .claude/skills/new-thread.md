---
description: Create a new background worker thread
---

Gather the following from the user:
1. Description of the task (required)
2. Working directory (optional, default: ~/.singleton/workers/default/)
3. Permissions mode: supervised/yolo/passthrough (default: supervised)

Then call `create_thread(description, cwd, permissions_mode)` and confirm with the thread ID.
