---
name: singleton
description: "Coordinate durable background agent sessions through singleton MCP tools."
---

# singleton foreground-agent cookbook

Use this skill when work can run independently, in parallel, or beyond the
current foreground turn. Prefer singleton for delegated implementation,
research, long-running checks, and recovery-friendly coordination. Do not use
it for a quick local command or edit that is simpler to complete directly in
the foreground session.

## Coordination loop

1. Call `get_capabilities` before assuming backend, host, workspace, or tool
   support.
2. Create one background session per independent task with `create_session`.
   For editing tasks, prefer an isolated `git_worktree` workspace with
   `cleanup_policy: "keep"` unless the user asked for a disposable workspace.
   Use `ensure_workspace` only when you intentionally need to create or reuse a
   workspace before sessions are created.
3. Start work with `send_message`. Treat the returned `turn_id` as
   asynchronous; store each `session_id`, `turn_id`, and latest event cursor.
4. Monitor sessions in a round-robin loop with
   `read_events({ session_id, cursor, wait_ms })` plus `get_inbox`. Do not
   block indefinitely on a single session.
5. When a turn completes, prefer
   `get_latest_output({ session_id, turn_id })` for final assistant output when
   `get_capabilities` advertises it. Fall back to `read_events` only when the
   tool is unavailable or its result says `needs_event_inspection: true`, then
   `ack_inbox` the handled completed or failed turn.
6. Resolve pending requests from `get_inbox` with `resolve_request`. Approve
   only when policy or the user permits it; deny unsafe or out-of-scope requests
   with a clear reason; answer input requests with `decision: "respond"` and
   the user-provided or policy-derived response.
7. Report outcomes, failures, and open questions in the foreground session.
8. Archive completed sessions with `close_resource`. Delete or dispose
   workspaces separately only when no active session still needs them, and avoid
   `force` unless the user explicitly approves destructive cleanup.

## Inbox handling

Use `get_inbox` as the fan-in primitive:

- `permission_request`: decide from policy or ask the user, then
  `resolve_request({ request_id, decision, response, reason })`.
- `input_request`: gather the missing input, then
  `resolve_request({ request_id, decision: "respond", response })`.
- `completed_turn`: call `get_latest_output({ session_id, turn_id })` when
  available, summarize the result, inspect raw events only if requested, then
  `ack_inbox({ turn_id })`.
- `failed_turn`: call `get_latest_output({ session_id, turn_id })` when
  available, inspect raw events only if requested, decide whether to retry,
  cancel, or report failure, then `ack_inbox({ turn_id })`.

Keep inbox summaries short. Acknowledge only after the foreground agent has
handled the item.

## Cancellation and correction

Cancel a turn when it is obsolete, runaway, or based on bad context:

1. Check current state with `get_session`.
2. Call `cancel_turn({ session_id, turn_id })`.
3. Read cancellation or failure events with `read_events`.
4. Send a corrected follow-up with `send_message` only if the session remains
   usable.

## Recovery after foreground context loss

A new foreground agent can recover coordination state without a singleton-owned
hub transcript:

1. Call `get_capabilities`.
2. Call `get_inbox`.
3. Call `list_sessions` to find running, idle, and needs-input sessions.
4. Call `get_session` for each relevant session.
5. Call `get_latest_output({ session_id })` for unread completed or failed
   turns when the tool is available.
6. Resume event cursors from the last known cursor. If no cursor survived, read
   the relevant session events from the beginning or latest safe checkpoint and
   rebuild the foreground summary.

Singleton owns orchestration state and workspace lifecycle. The backend owns
canonical conversation persistence, and the filesystem owns source files and
git state.
