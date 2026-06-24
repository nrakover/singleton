# Foreground Agent Coordination Guide

`singleton` does not own a foreground hub session. A foreground agent becomes
the hub by convention: it uses singleton's MCP tools to create background
sessions, dispatch turns, watch events, resolve requests, and clean up
resources.

The Copilot CLI plugin packages this cookbook as the `singleton` Skill in
`.github/plugin/skills/singleton/SKILL.md` so installed foreground agents can
load it directly.

## Coordinator prompt

Use this as a system/developer-style instruction for an MCP-capable foreground
agent:

```text
You coordinate background agent sessions through singleton MCP.

Use singleton for work that can run independently or outlive the current
foreground turn. Prefer isolated git worktree workspaces for editing tasks.
Use shared workspaces only for intentionally read-only/research tasks.

Default loop:
1. Call get_capabilities before assuming host, backend, or workspace support.
2. Create or reuse workspaces with ensure_workspace when workspace intent
   matters; otherwise pass a WorkspaceSpec inline to create_session.
3. Create one session per independent task.
4. Send work with send_message. Treat the returned turn_id as asynchronous.
5. Poll read_events and get_inbox. Do not block indefinitely on one session.
6. Resolve permission/input requests with resolve_request after applying policy
   or asking the user.
7. Summarize completed turns, failures, and open questions.
8. Close sessions and disposable workspaces with close_resource.

Do not assume singleton owns the backend transcript. Use singleton events for
orchestration state and ask the backend session for conversation details only
through supported tools/events.
```

## Fresh worktree editing task

Use when a background session will modify a repository.

```text
1. get_capabilities()
2. create_session({
     description: "Implement parser validation tests",
     workspace: {
       kind: "git_worktree",
       repo: "/path/to/repo",
       base_ref: "main",
       cleanup_policy: "keep"
     },
     labels: ["parser", "tests"]
   })
3. send_message({
     session_id,
     message: "Add parser validation tests and run the relevant test target."
   })
4. read_events({ session_id, cursor, wait_ms: 30000 })
5. get_inbox()
6. close_resource({ target: { session_id }, disposition: "archive" })
```

Cleanup should target the workspace separately:

```text
close_resource({
  target: { workspace_id },
  disposition: "delete",
  force: false
})
```

If active sessions still reference that workspace, singleton should refuse the
delete unless `force` is explicit.

## Parallel research

Use when sessions can inspect the same codebase without editing it.

```text
1. ensure_workspace({
     spec: {
       kind: "git_worktree",
       repo: "/path/to/repo",
       base_ref: "main",
       cleanup_policy: "keep"
     }
   })
2. create_session({ description: "Research auth flow", workspace: { kind: "existing_workspace", workspace_id } })
3. create_session({ description: "Research storage flow", workspace: { kind: "existing_workspace", workspace_id } })
4. create_session({ description: "Research MCP flow", workspace: { kind: "existing_workspace", workspace_id } })
5. send_message(...) for each session
6. Round-robin read_events({ session_id, cursor, wait_ms: 1000 }) across sessions
7. Merge findings in the foreground response
8. close_resource({ target: { session_id }, disposition: "archive" }) for each session
```

## Inbox handling loop

`get_inbox` is the foreground agent's fan-in primitive.

```text
loop:
  inbox = get_inbox()
  if inbox.items is empty:
    break

  for item in inbox.items:
    if item.kind == "permission_request":
      decide from policy or ask user
      resolve_request({ request_id: item.request_id, decision, response, reason })

    if item.kind == "input_request":
      ask user if needed
      resolve_request({ request_id: item.request_id, decision: "respond", response })

    if item.kind == "failed_turn":
      read_events({ session_id: item.session_id, cursor: last_cursor })
      decide whether to retry, cancel, or report failure
      ack_inbox({ turn_id: item.turn_id })

    if item.kind == "completed_turn":
      read_events({ session_id: item.session_id, cursor: last_cursor })
      summarize the result
      ack_inbox({ turn_id: item.turn_id })
```

Keep inbox summaries short. Use `read_events` for detail, then acknowledge
handled turn items so they do not stay in the unread inbox.

## Approval policy prompt

Use this prompt when the foreground agent can ask the user about a pending
request:

```text
Background session {session_id} requests permission:

{summary}

Approve once, deny, or provide a narrower instruction?
```

Resolution examples:

```text
resolve_request({
  request_id,
  decision: "approve",
  response: { "scope": "once" }
})

resolve_request({
  request_id,
  decision: "deny",
  reason: "The command would modify files outside the assigned worktree."
})
```

## Cancellation pattern

Use cancellation when a turn is obsolete, runaway, or blocked on bad context.

```text
1. get_session({ session_id })
2. cancel_turn({ session_id, turn_id })
3. read_events({ session_id, cursor, event_types: ["turn.cancelled", "turn.failed"], wait_ms: 30000 })
4. Send a corrected follow-up turn only if the session remains usable.
```

## Recovery after context loss

When a new foreground agent takes over:

```text
1. get_capabilities()
2. get_inbox()
3. list_sessions({ statuses: ["running", "idle", "needs_input"] })
4. get_session({ session_id }) for each relevant session
5. read_events({ session_id, cursor: latest_known_cursor })
```

The foreground agent should not need a singleton-owned hub transcript to
recover coordination state.
