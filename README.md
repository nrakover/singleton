# singleton

`singleton` lets your foreground agent coordinate durable background agent
sessions. It gives tools like Copilot CLI a small MCP control plane for starting
background work, checking results later, handling approvals, and recovering state
after the foreground chat exits.

Use it when you want your current agent to delegate independent work without
losing track of the sessions it started.

## Quick start with Copilot CLI

There are two pieces to install:

1. The native `singleton` binary, which gives you admin commands such as
   `singleton status` and `singleton stop`.
2. The foreground-agent plugin, which teaches Copilot how to use singleton over
   MCP and installs the `singleton` Skill.

### 1. Install the binary

Download the latest archive for your platform from
<https://github.com/nrakover/singleton/releases/latest>.

Current prebuilt archives:

- `singleton-aarch64-apple-darwin.tar.gz` for macOS Apple Silicon
- `singleton-x86_64-unknown-linux-gnu.tar.gz` for Linux x86_64

Install it somewhere on your `PATH`, for example:

```bash
mkdir -p "$HOME/.local/bin"
tar -xzf singleton-aarch64-apple-darwin.tar.gz
install -m 0755 singleton-*/singleton "$HOME/.local/bin/singleton"
singleton status
```

Use the Linux archive name instead on Linux. If `singleton` is not found after
installation, add your local bin directory to your shell startup file:

```bash
echo 'export PATH="$HOME/.local/bin:$HOME/.cargo/bin:$PATH"' >> ~/.zshrc
exec zsh
```

For bash, use `~/.bashrc` instead of `~/.zshrc`.

If you prefer building from source:

```bash
rustup toolchain install 1.94.0 --profile minimal
cargo +1.94.0 install --locked \
  --git https://github.com/nrakover/singleton \
  singleton-cli --bin singleton
```

### 2. Install the Copilot plugin

```bash
copilot plugin marketplace add nrakover/singleton
copilot plugin install singleton@singleton
```

The plugin contributes:

- a `singleton` MCP server for Copilot CLI
- a `singleton` Skill with the foreground-agent coordination cookbook

Start a new Copilot CLI session and ask it to use singleton for background work.
For example:

```text
Use the singleton skill. Create a background session that replies exactly
"singleton ok", then show me the latest output.
```

## Everyday commands

```bash
singleton status              # show daemon state and known sessions
singleton stop                # stop the local daemon and clean stale files
singleton start               # start/reuse the daemon explicitly
singleton mcp-config          # print a manual MCP config snippet
```

`singleton serve --stdio` is the MCP entrypoint used by foreground-agent
clients. It starts or reuses the local daemon and proxies MCP traffic to it, so
background turns can keep running after the foreground client disconnects.

## Other foreground agents

After the binary is on your `PATH`, singleton can register itself with other MCP
clients:

```bash
singleton install-mcp --client copilot
singleton install-mcp --client claude
singleton install-mcp --client codex
```

Use `--dry-run` to print the native client command without running it.

## How it works

Singleton owns orchestration state: sessions, turns, inbox items, requests,
workspaces, daemon lifecycle, and a normalized event index. The agent backend
owns the canonical conversation transcript. The filesystem and git own source
files, commits, and untracked changes.

The current MVP is local-first and Copilot-backed:

- Rust `singleton` daemon and CLI
- SQLite durable state
- MCP tool surface for foreground agents
- GitHub Copilot SDK backend
- local workspace and git worktree support
- deterministic fake backend for tests

## More documentation

- `docs/installation.md` for detailed install options and plugin overrides
- `docs/foreground-agent-coordination.md` for the coordination model
- `spec/` for behavioral and interface specs
- `AGENTS.md` for contributor/agent development guidance
