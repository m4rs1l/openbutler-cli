# openbutler-cli

The command-line client for OpenButler. It is one binary, `openbutler`, that
speaks the framed protocol owned by
[openbutler-protocol](https://github.com/m4rs1l/openbutler-protocol) to a host
started by [openbutler-daemon](https://github.com/m4rs1l/openbutler-daemon).

The split is deliberate: the host computes and decides, the client prints. A
command here turns into one request, and the screen it shows — tables, tones,
sentences — is composed by the host and carried back line by line. The client
applies its glyph palette and nothing else.

## Install

Linux (x86_64 or arm64):

```sh
curl -fsSL https://raw.githubusercontent.com/m4rs1l/openbutler-cli/main/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/m4rs1l/openbutler-cli/main/install.ps1 | iex
```

The installer downloads the release archive for this platform, verifies it
against `SHA256SUMS`, installs `openbutler` into `~/.openbutler/bin`
(`%USERPROFILE%\.openbutler\bin`), checks the installed binary, and writes a
marker so a second run is a no-op. `--version`, `--prefix`, `--no-modify-path`
and `--quiet` behave as they do in the host installer. Installing the host into
the same prefix puts `openbutlerd` beside the client, which is where the client
looks for it.

## Requirements

- Rust 1.99.1 (pinned by `rust-toolchain.toml`; rustfmt and clippy included).
- A running host. `openbutler daemon start` spawns the `openbutlerd` binary that
  sits next to this one, so an install ships both.
- Node.js on the host machine: the daemon runs the model sidecar with it.
  The CLI itself needs no network stack beyond TCP to a remote host.

## Build

```sh
cargo build --release
./target/release/openbutler --version      # openbutler 0.0.1 (cli)
```

`openbutlerd` is built from the daemon repository; put the two binaries in the
same directory and the client finds its host.

## Quick start

Local host, first run:

```sh
openbutler daemon start                 # starts openbutlerd next to this binary
openbutler admin create <username>      # the account remote devices sign in with
openbutler status
openbutler run "say hello"              # one request, streamed, exits
openbutler tui                          # interactive session
```

Remote host (the daemon must be started with `--listen <tailnet-ip>:<port>`,
and the listener refuses to open until an admin account exists):

```sh
openbutler connect <host>:<port> <username>   # password prompted, masked
openbutler status                             # now answers from that host
openbutler connect --forget                   # back to the local host
```

## Commands

| Command | What it does |
|---|---|
| `status` | The host's report and screen |
| `sessions [prune --older-than <window>]` | Saved sessions, or prune them |
| `run <request>` | One request, streamed to the terminal |
| `tui` | Interactive session |
| `daemon start\|stop\|restart\|status\|run` | This machine's host process |
| `daemon install\|uninstall` | The login entry that starts the host at sign-in |
| `uninstall [--purge]` | Remove the program; the data stays unless `--purge` |
| `logs` | The machine log's tail (`--level`, `--event`, `--json`) |
| `usage` | Recorded token use and cost |
| `events`, `actions` | The durable ledgers, with `retry` and `prune` |
| `config` | Providers, default model, readiness |
| `setup` | First-run wizard (interactive) |
| `admin create <username>` | The account that signs devices in |
| `connect <host:port> <username>` | Sign this client in to a host, `--forget` to drop it |

| `skills`, `schedule`, `mcp`, `keys`, `permissions`, `web`, `about` | Their own surfaces |

Two rules decide where a command goes:

- `openbutler daemon …` always talks to **this machine's** host, because it
  manages that process.
- Everything else follows the stored backend: after `connect`, commands answer
  from that host. There is no silent fallback — a stored backend that fails to
  authenticate is an error, never a quiet question to the local socket.

## Where things live

- **Data directory**: `OPENBUTLER_DATA_DIR` when set, otherwise the platform
  default (`%LOCALAPPDATA%\openbutler-daemon` on Windows,
  `~/Library/Application Support/openbutler-daemon` on macOS,
  `$XDG_DATA_HOME/openbutler-daemon` on Linux). The name is shared with the
  host on purpose: both resolve the same directory by the same rule.
- **Stored backend**: `backend.json` (host, port, username, device — no secret)
  and `credential.json` (the session token, written private). Both live in the
  client's data directory. The token is the client's own identity; the host
  never reads these files.

## Status

Pre-release, version 0.0.1: the wire is protocol version 2 and consumers pin a
git revision. The surface grows one slice at a time; every command above is
pinned by end-to-end tests against a contract-shaped fake host.

## License

MIT. See [LICENSE](LICENSE).
