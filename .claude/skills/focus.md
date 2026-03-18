---
description: Focus on a specific thread
---

Ask the user for a thread ID (or let them pick from list_threads()). Then call:
1. `get_thread(thread_id)` for metadata
2. `thread_output(thread_id, page=0)` for recent output
3. `get_thread_events(thread_id, page=0)` for recent events

Set context for continued work with that thread.
