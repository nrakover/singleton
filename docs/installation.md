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

The plugin contributes one MCP server named `singleton`. Its launcher keeps MCP
stdout reserved for JSON-RPC, writes bootstrap diagnostics to stderr, installs a
release binary into `${COPILOT_PLUGIN_DATA}/bin` when needed, then execs:

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

The plugin currently supports macOS Apple Silicon and Linux x86_64 release
archives. Other platforms should install from source and set `SINGLETON_BINARY`
if they want to use the Copilot plugin launcher.

Current Copilot CLI versions can also install repository plugins directly, for
example `copilot plugin install nrakover/singleton:.github/plugin`, but Copilot
warns that direct installs are deprecated. Prefer the marketplace flow above.

## Direct binary installation

Rust users can install from source:

```bash
cargo install --locked --git https://github.com/nrakover/singleton --bin singleton
```

Tagged releases publish prebuilt archives named:

```text
singleton-aarch64-apple-darwin.tar.gz
singleton-x86_64-unknown-linux-gnu.tar.gz
```

Each archive has a matching `.sha256` file.

## Registering MCP clients

After `singleton` is on PATH, register it with a supported client:

```bash
singleton install-mcp --client copilot
singleton install-mcp --client claude
singleton install-mcp --client codex
```

Use `--dry-run` to print the exact command instead of running it. Use
`--binary /path/to/singleton`, `--backend`, `--database`, or `--name` to
customize the registered server.

The generated client commands are:

```bash
copilot mcp add singleton -- singleton serve --stdio --backend copilot
claude mcp add --transport stdio singleton -- singleton serve --stdio --backend copilot
codex mcp add singleton -- singleton serve --stdio --backend copilot
```

`singleton mcp-config --backend copilot` prints a JSON snippet for clients that
need manual MCP configuration.
