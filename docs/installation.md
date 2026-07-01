# Installing singleton

`singleton` has two distribution layers:

1. A Rust CLI binary named `singleton`.
2. MCP/client configuration that points a foreground agent at
   `singleton serve --stdio --backend copilot`.

## Copilot CLI plugin

The preferred Copilot CLI path is:

```bash
copilot plugin marketplace add nrakover/singleton
copilot plugin install singleton@singleton
```

The plugin contributes one MCP server named `singleton` and one Skill named
`singleton` that teaches foreground agents the coordination loop. Its launcher
keeps MCP stdout reserved for JSON-RPC, writes bootstrap diagnostics to stderr,
installs a release binary into `${COPILOT_PLUGIN_DATA}/bin` when needed, then
execs:

```bash
singleton serve --stdio --backend copilot
```

Supported launcher overrides:

| Variable | Purpose |
|---|---|
| `SINGLETON_BINARY` | Use an explicit binary path instead of downloading a release. |
| `SINGLETON_VERSION` | Download a specific tag such as `v0.1.0` instead of the latest release. |
| `SINGLETON_RELEASE_BASE_URL` | Download archives from a custom base URL. |
| `SINGLETON_FORCE_INSTALL=1` | Reinstall the release binary even if one already exists. |
| `SINGLETON_BACKEND` | Override the backend passed to `serve`; defaults to `copilot`. |
| `SINGLETON_DATABASE` | Pass an explicit singleton SQLite database path. |
| `SINGLETON_CONFIG` | Use an explicit singleton config file. |
| `SINGLETON_PROFILE` | Select a named singleton config profile. |
| `SINGLETON_NO_PROJECT_CONFIG=1` | Disable nearest-ancestor `.singleton.toml` loading. |

The plugin currently supports macOS Apple Silicon and Linux x86_64 release
archives. Other platforms should install from source and set `SINGLETON_BINARY`
if they want to use the Copilot plugin launcher.

Current Copilot CLI versions can also install repository plugins directly, for
example `copilot plugin install nrakover/singleton:.github/plugin`, but Copilot
warns that direct installs are deprecated. Prefer the marketplace flow above.

## Direct binary installation

The preferred direct binary installer is hosted as a GitHub Release asset:

```bash
curl -fsSL https://github.com/nrakover/singleton/releases/latest/download/install.sh | bash
```

It supports macOS Apple Silicon and Linux x86_64, downloads the matching release
archive plus `.sha256` file, verifies the checksum, and installs
`singleton` into `$HOME/.local/bin` by default. It never invokes `sudo` or edits
shell startup files.

Installer options can be passed with `bash -s --`:

```bash
curl -fsSL https://github.com/nrakover/singleton/releases/latest/download/install.sh \
  | bash -s -- --version v0.1.0 --install-dir "$HOME/.local/bin"
```

Supported installer controls:

| Option/env | Purpose |
|---|---|
| `--version VERSION` / `SINGLETON_VERSION` | Install a specific tag such as `v0.1.0`. |
| `--install-dir PATH` / `SINGLETON_INSTALL_DIR` | Install directory; defaults to `$HOME/.local/bin`. |
| `--release-base-url URL` / `SINGLETON_RELEASE_BASE_URL` | Download archives from a custom base URL. |
| `--force` / `SINGLETON_FORCE_INSTALL=1` | Reinstall even if the target version is already installed. |
| `--dry-run` | Print the resolved platform, URLs, and target path without downloading. |

Tagged releases publish prebuilt archives named:

```text
singleton-aarch64-apple-darwin.tar.gz
singleton-x86_64-unknown-linux-gnu.tar.gz
```

Each archive has a matching `.sha256` file.

Rust users can also install from source:

```bash
cargo install --locked --git https://github.com/nrakover/singleton --bin singleton
```

## Updating the binary

Use `singleton update` to update an existing direct binary installation from
GitHub Releases:

```bash
singleton update
singleton update --version v0.1.0
singleton update --dry-run
singleton update --install-dir "$HOME/.local/bin"
```

`singleton update` uses the same archive names and checksum files as the
installer. By default it updates the currently running `singleton` executable;
`--install-dir` targets `PATH/singleton` instead. The command downloads into a
temporary directory, verifies the checksum before extraction, validates the
candidate binary with `--version`, skips replacement when the installed version
is already current unless `--force` is set, then replaces the target binary via
a same-directory temporary file and rename.

The command does not escalate privileges. If the target is not writable, choose a
user-writable install directory or rerun the installer with an explicit
`--install-dir`. Updating the binary does not restart an already-running
`singletond`; stop it with `singleton stop` when you want the next daemon start
to use the new binary.

## Registering MCP clients

After `singleton` is on PATH, register it with a supported client:

```bash
singleton install-mcp --client copilot
singleton install-mcp --client claude
singleton install-mcp --client codex
```

Use `--dry-run` to print the exact command instead of running it. Use
`--binary /path/to/singleton`, `--backend`, `--database`, `--config`,
`--profile`, `--no-project-config`, or `--name` to customize the registered
server.

The generated client commands are:

```bash
copilot mcp add singleton -- singleton serve --stdio --backend copilot
claude mcp add --transport stdio singleton -- singleton serve --stdio --backend copilot
codex mcp add singleton -- singleton serve --stdio --backend copilot
```

`singleton mcp-config --backend copilot` prints a JSON snippet for clients that
need manual MCP configuration.

## Singleton configuration

Singleton reads persistent preferences from TOML. On macOS/Linux the default
user config path is:

```text
${XDG_CONFIG_HOME:-$HOME/.config}/singleton/singleton.toml
```

Project config is the nearest ancestor `.singleton.toml` from the invocation
directory. Disable project config with `--no-project-config` or
`SINGLETON_NO_PROJECT_CONFIG=1`. Override the user config path with
`--config PATH` or `SINGLETON_CONFIG=PATH`, and select a profile with
`--profile NAME` or `SINGLETON_PROFILE=NAME`.

Daemon state defaults to `~/.singleton`; the default database is
`~/.singleton/singleton.db`. `--database` and `SINGLETON_DATABASE` still provide
explicit state isolation.

If no config file exists, singleton behaves as if this config were present:

```toml
version = 1
default_profile = "default"

[profiles.default]
backend = "copilot"
mode = "interactive"
state_dir = "~/.singleton"
database = "~/.singleton/singleton.db"
default_host = "host_local"
repo_workspace_provider = "git_worktree"
cleanup_policy = "keep"

[profiles.default.permissions]
default = "ask"

[hosts.host_local]
kind = "local"
```

`mode` controls the backend/agent execution mode. `permissions.default` controls
singleton-managed permission/input request policy.
`repo_workspace_provider = "git_worktree"` means repo-backed shorthand
workspaces default to isolated git worktrees, while ordinary non-git
directories fall back to `local_path`.

SSH hosts delegate normal SSH details to your central SSH config:

```toml
[hosts.devbox]
kind = "ssh"
target = "devbox"
connect_command = "singleton serve --stdio"
ssh_args = ["-o", "BatchMode=yes"]
```

`target` is the exact SSH target or alias. `connect_command` defaults to
`singleton serve --stdio`. Do not put raw passwords, tokens, or private-key
contents in singleton config; singleton state stores only safe references and
metadata. Project config may use the default connect command, but cannot set or
inherit non-default `connect_command` values or `ssh_args` for SSH hosts.
SSH host descriptors are parsed for future support; they do not make remote
workspace/session placement available yet.
