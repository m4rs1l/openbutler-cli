# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `openbutler uninstall [--purge] [--dry-run] [--force]`: removes the program
  the installer laid down, its login entry and its PATH line. The data
  directory is kept unless `--purge`; a directory without the installer marker
  is never deleted, a login entry that belongs to another command is left
  alone, and a binary that holds its own directory open is removed by a watcher
  once the process exits.

- `openbutler daemon install [--web]` and `daemon uninstall`: the per-user
  login entry that starts the host at sign-in - the Run key on Windows, a
  systemd user unit on Linux. The entry runs `openbutlerd`, never the client,
  removal is idempotent, and an entry that already belongs to another command is
  refused rather than overwritten.

- `openbutler approve [<n>]`: the auto-review denials the host numbered, and
  the one-shot retry granted for one of them. The screen is the host's; the
  client sends the number and prints the answer.

- `openbutler --list-models [--filter <term>] [--limit <n>] [--provider <id>]`:
  the source's top-level flag set. The host composes the screen
  (`catalog.listToolModels`) and the client prints it, so the two can never
  disagree about which model can call a tool.

- Top-level `openbutler stop` and `openbutler restart`: the short
  spellings run the same flows as `daemon stop` and `daemon restart`.

- `setup` reads the host catalog: a provider picker (name, id, model count)
  with a custom-endpoint escape, then a model picker for the chosen provider,
  and the header prints the host paths instead of the client paths. A
  catalog provider saves its package (`npm`) and no URL.

- Component installers (`install.sh`, `install.ps1`): select the release for
  the platform, verify it against `SHA256SUMS`, unpack into
  `~/.openbutler/bin` (`%USERPROFILE%\.openbutler\bin`), run the installed
  binary to confirm it reports the requested version, and write a marker so a
  second run is a no-op. `--version`, `--prefix`, `--no-modify-path` and
  `--quiet` are supported; `--prefix` lets the new stack live beside a legacy
  install until the user is ready to switch.
- A release workflow: one tag publishes three platform archives
  (`openbutler-<version>-<target>`) with `SHA256SUMS`. The tag must equal the
  workspace version, and the built binary must answer `openbutler --version`
  before the release is created.
- `connect <host:port> <username>`: signs this client in to a host. The
  password is prompted (masked, terminal only) and never an argument, because
  argv is visible in the process list and lands in shell history. On success it
  stores `backend.json` (the target, no secret) and a private
  `credential.json` (the session), then prints the host's own status screen
  and one line about its own file. `--forget` drops both.
- `admin create <username>`: creates the admin account through the local host,
  asking for the password twice; the confirmation never leaves the terminal.
- A stored backend decides where later commands dial: `Client::connect`
  presents the stored session as the connection's first frame. `daemon
  start|stop|restart|run` keeps dialing the local endpoint, because those verbs
  are about this machine's process.
- `daemon start --web` passes `--web` to the spawned host, which then
  serves the browser UI; `openbutler web` prints its pair URL. A host that
  is already running is reported unchanged.
- `setup`'s Review block is the host's screen: the wizard asks
  `settings.setup.review` for the set it collected and prints the answered
  lines with its glyph palette. The body words and the "no provider
  configured" sentence moved to the daemon; the CLI keeps only the prompts
  it needs to collect the answers.
- `openbutler mcp [list|add|remove]`: the host's list screen, the source's
  interactive add/remove flows, and the "use an interactive terminal" line
  when there is none. The payload field is `command` because the envelope
  already owns `action` (a payload key of the same name fails to decode).
- `openbutler logs --level --event --json`: the filters travel to the host
  (`logs.tail`), which selects and formats the records.
- `openbutler sessions prune --older-than D [--yes]` — session retention
  through the host: the preview lists each record with its UTC date, id
  and size plus the source's preview sentence; `--yes` deletes and prints
  `Deleted N session file(s) (…).` with a `N file(s) could not be
  deleted.` tail. Every line arrives with the host's tone and this module
  only applies its glyph palette (`ui::toned`). `--older-than` is
  required by the parser (exit 2), and the daemon's duration refusal
  surfaces verbatim.
- `openbutler keys [add <provider>|list|remove <provider>]` — the provider
  key store through the daemon: the table with display tails, the two-line
  empty screen, masked key collection (the setup wizard's prompts, one
  authority) and a confirmed remove. Both write actions refuse without a
  terminal.
- `openbutler skills [--workspace PATH]` — the Agent Skill table (or the
  source's empty screen naming every scanned root) from the daemon's
  `skills.list`; discovery and frontmatter rules live in the host.

- `openbutler about` — version, shell, runtime and the configuration
  paths from the daemon's `about.report` (the CLI prints the answer and
  adds nothing but its own version).
- `openbutler events` / `openbutler actions` — the durable ledgers: a bare call prints the daemon's rows as pretty JSON (the source's dump shape), `events retry <id>` sends `{eventId}`, and `prune --older-than <d> [--yes]` asks `ledger.prune` and lays out its counts (preview without `--yes`, the source's re-run hint; `No <kind> records…` when nothing is old enough). Pins: the pretty-JSON dump, the retry payload, both prune renders and their payloads, and the empty note. Verified live: empty ledgers, a preview, an invalid duration, stop.
- `openbutler permissions [workspace|auto-review|full-access]
  [--review-timeout N] [--approval-timeout N] [--review-model P/M|none]` —
  one `permissions.manage` call carries every provided flag; the screen is
  the source's (four `kv` values, two explanations, one success line per
  flag, the restart note naming `openbutler daemon restart`). The
  auto-review history section is not ported — no verdicts are recorded in
  this build. Pins: the bare render (value byte 25, line count), the full
  payload (`{mode, reviewTimeoutSeconds, approvalTimeoutSeconds,
  reviewModel}`) with its success lines, and the refusal pass-through.
  Verified live: bare, set, invalid-value refusal, stop.
- `openbutler daemon token [create|list|revoke <id>]` — remote auth tokens
  (bare `token` lists). The source's four `create` lines, one `kv` row per
  token for `list` (or the source's empty note), `Token <id> revoked.` for
  `revoke`. The store is the daemon's (`auth-tokens.json`, salted hashes;
  the CLI never reads it) and the refusals are the daemon's sentences,
  rendered verbatim. Pins: the four `create` lines (value byte 25, the
  `ui.kv` contract), list rows and the empty note, the revoke payload
  (`{tokenId}`) and the refusal pass-through. Verified live against a real
  host: create, list, revoke, list-again, stop.
- `openbutler daemon run [--listen HOST:PORT | --no-listen]` — the host in
  the foreground: the CLI runs the daemon executable with inherited stdio
  and hands back its exit status, so the start lock, the listener
  validation and the serve loop all stay daemon-side. Deliberate
  divergence: the source ran the host in-process; this stack's host is its
  own binary. Pins: the child's status is handed back (a real executable
  exiting 1), and a missing executable is the start sentence. Verified
  live: the foreground host answers `status`, stops on `daemon stop`, and
  the CLI exits 0 with the daemon's IPC line on stderr.
- `openbutler daemon restart` — the source's one restart flow: the stop
  half asks the host's `stop` with the source's `restart` flag over one
  connection (so every other client hears the restart wording), waits at
  250 ms for the endpoint, then starts again and prints the source's two
  closing lines. The flow tolerates a host that is not running and keeps
  the saved listener. Divergences, documented in the module: no separate
  web process in this build, so the source's browser-server lines have
  nothing to report. The listener hint now names the command this surface
  actually ships (`openbutler daemon restart`). Pins: the stop payload
  (`{"restart": true}`) against the fake daemon, the not-running
  tolerance, and the rendered closing lines.
- `openbutler tui` — the interactive session (ratatui + crossterm, slim
  first). Three parts with one job each: `tui::model` decides (pure —
  keys, intents, patch application), `tui::render` draws (pinned through
  ratatui's `TestBackend`), and `tui` owns the terminal and the socket.
  The screen opens on the saved-session list and never attaches or creates
  a chat by itself (the no-default-session ruling); a chat streams frames
  into the transcript, answers approvals with `y`/`n` and questions with
  the input line, and drops frames that belong to another chat. A
  non-terminal caller gets the refusal sentence before any terminal is
  touched. Pins: seven model tests (snapshot open, delta append + text
  continuation, approval keys, question answers and dismissal, the list
  never opening a chat, selection clamping), four render tests, the
  terminal guard and one end-to-end refusal through the command tree.
- `openbutler daemon start --listen HOST:PORT` / `--no-listen` — the
  remote listener preference. The write is the daemon's
  (`settings.set-listen`, validated by the rule the host binds by): a cold
  start passes the flag to the child and persists it once the host
  answers, a running host persists and says the source's line
  (`The saved listener applies on the next `openbutler daemon restart`.`, exit
  0). `--no-listen` clears it and prints the source's cleared note. Both
  halves pinned end to end against the fake daemon (payload + rendered
  lines).
- `openbutler config` — the manager screen, rendered entirely from the
  daemon's `config.report`: MCP servers, approval-mode
  label, the provider table (stored order, kind, keys, tool counts, the
  default row's model), the default pair with `(from usage)`, the
  reasoning line and readiness with the source's sentences. Divergences
  are documented in the module: the daemon starts
  the catalog download instead of blocking on it (a first run shows
  `unknown` plus a note), the no-default hint says `run`, MCP rows are
  id-sorted. Three unit pins (full screen byte for byte, empty machine,
  usage default + download note) and two end-to-end pins over the fake
  daemon (render, refusal sentence).
- A shared `ui` module (section bar, key/value line, markers, table — the
  `ui.ts` shapes, trailing `padEnd` included) is now the one render
  authority: `setup` moved onto it and `config` was built on it. Five
  pins.
- `openbutler setup` — the first-run wizard. Prompts collect the
  provider id, required URL, masked keys and a free-text default model;
  the review builds one `settings.setup` payload and the daemon writes
  both documents (backend-owned: the source's client-side file write is
  not ported). The terminal guard answers the source sentence when stdin
  is not a terminal (exit 1); cancel is Ctrl+D/EOF or the discard choice.
  Divergences are documented in the module:
  custom endpoints only, no npm prompt, plain numbered menus, closing
  hint says `run`. Five unit pins (transcript, refusals, discard/EOF,
  render helpers, guard) plus two end-to-end pins (guard exit over the
  real binary, payload against a contract-shaped fake daemon).
- CLI slice S1: `openbutler status` and `openbutler sessions` over the
  framed local pipe, consuming only what the daemon computes. The client
  derives the endpoint from `openbutler-protocol::endpoint` (one dial
  rule with the daemon), correlates response ids, and validates the
  envelope; renders format `DaemonStatus` and `SessionSummary` without
  deriving anything. Bare `openbutler` prints usage (exit 2) — unknown
  verbs never become prompts. Six end-to-end pins run against a
  contract-shaped fake daemon on the derived endpoint (success
  rendering, the running marker, empty list, verbatim refusals,
  correlation refusal, absent-daemon message).
- CLI slice S2: `openbutler run "<request>"` — the one-shot lane. A
  fresh connection attaches through the daemon's new-chat bootstrap
  (daemon `43be9f4`), the request goes out, and the reply streams as
  patch events arrive (header block, assistant deltas by absolute
  index, tool lines, notice tails); approval and question prompts are
  answered over the same socket while rendering continues. The start
  window (5 s) and stall window (10 min) keep the legacy one-shot
  semantics verbatim, including `session.stop` on stall, and both
  failure sentences are pinned. Three new end-to-end pins (nine total):
  byte-exact streaming stdout, the stall path, and the never-started
  path.
- CLI slice S3 (first part): `openbutler daemon status` (bare `daemon`
  defaults to it; shares the render authority with `status`) and
  `openbutler daemon stop` — the host's own `stop` action, then a 250 ms
  poll until the endpoint leaves; a missing host is a normal answer,
  and the 30 s timeout returns the source's identity-refusal sentence
  because this build keeps no pid record. Three new pins (twelve e2e
  total), including a fake that leaves the endpoint once its script is
  fulfilled.
- CLI slice S3: `openbutler daemon pair` mints through the host's
  `pair.create` and renders the source's three lines (code, local
  expiry clock via chrono, canonical redeem form). The redeem side
  (saved remote backends) is deferred as its own slice; the owner
  delegated the signature decision (2026-11-22). One new pin (thirteen
  e2e total), shape-pinning the timezone-dependent expiry line.
- CLI slice S3: `openbutler daemon start` — spawn-or-report over the
  pipe: an active host returns the start block untouched; otherwise the
  executable resolves (`OPENBUTLER_DAEMON` then the sibling layout),
  spawns `run` with `daemon.log` as output, and polls status at 250 ms
  against a 20 s deadline with a kill on timeout. Early death is
  diagnosed from the log tail (4000-byte window, source `readDaemonLogSince`
  semantics): OOM sentences retry with the source's 750/2000 ms backoff,
  deterministic faults (bind clash, panic, bad invocation) answer at
  once, empty tails point at the log file. Deliberate divergences are
  listed in the command doc: no model-readiness pre-check (backend-owned
  rule; readiness belongs in status) and no `--listen`/`--web` flags
  yet. Three new pins (sixteen e2e total): the active-host block, the
  death diagnosis, the missing-executable sentence.
- CLI slice S3: `openbutler logs` tails the machine log by seeking
  backwards (64 KB blocks until the newline count is covered — the
  source read the whole file), with `--lines` (clap range 1..=100000)
  and the source's `--file` override; a missing or unreadable file is
  the empty answer. The default file is the `daemon.log` that
  `daemon start` captures because this build's daemon writes no
  structured `logs/openbutler.log` yet — level/event/`--json`
  filtering is deferred with that daemon-side logger (documented).
  Three new pins (nineteen e2e total): CRLF tailing, the missing-file
  sentence, and clap's range rejection.
- CLI slice S3: `openbutler usage` — the recorded spend as the source
  renders it. The CLI opens the host's `state.db` read-only (the
  source's contract: asking costs nothing and changes nothing; busy
  timeout matches better-sqlite3's 5 s default) and ports
  `usage/report.ts` faithfully: buckets keyed by each row's own
  `tz_offset_min` (never this machine's clock), fresh-input split
  (`no_cache_tokens ?? input - cache`), the summary with first-wins
  busiest ties, UTC-walk streaks, cost-source classification, JS
  `toFixed` rounding (multiply/round/divide in f64 first), and the
  sparkline. `--days` is clap usage per the failure contract; a
  missing ledger file is no rows, not an error. Four new pins
  (twenty-three e2e total): the exact render with an offset-rollover
  row, the empty sentence, the session/model scope, and the `--json`
  envelope shape.

### Removed

- `openbutler workspace default [path]`: the default workspace is gone. A
  session that names no directory works in the daemon's per-session scratch,
  so there is nothing to choose.

### Changed

- `run` prints the answer, not a banner: the model/provider header and the
  "your request will be sent to ..." line are gone. Reasoning continuations
  are labelled by the host and are never printed into the answer.

- `run` prints only the answer. Tool rows and their results stay in the
  transcript: the one-shot lane no longer writes `Tool: <name>` and the raw
  result into stdout, so a run that calls a tool reads like an answer.
  Two answer rows — a preamble before a tool call, then the answer — each
  start on their own line instead of running together.

- The client resolves a connection in one place: a stored backend first, the
  local endpoint otherwise, with no silent fallback between them — a stored
  backend that fails to authenticate is an error, never a quiet question to the
  local socket.
- `openbutler --version` prints `openbutler <version> (cli)`, and the run
  lines in the README keep one command name per install.
- `daemon start` looks for the sibling `openbutlerd` (+ `.exe` on Windows);
  the package and the data directory keep their names, so existing data is
  untouched.
- `skills` prints the host table or its empty screen; the CLI no longer
  lays it out.
- `status` and `sessions` print the host's screens (`DaemonStatus.lines`,
  `SessionListAnswer.lines`); a host that predates the screen says so
  instead of printing nothing. The protocol's `ScreenLine`/`Tone` is the
  one line shape, and `ui::screen` is the one glyph palette.
- `about` and `config` print the host's finished lines; the CLI no longer
  lays out either screen (the config layout — sections, provider table,
  readiness block — left this crate for the host).
- `openbutler logs` and `openbutler permissions` no longer decide anything:
  the host reads the log file (`logs.tail`, absolute path, local-only) and
  composes the permissions screen (`lines` with tones). The client applies
  its glyph palette (`ui::screen`) and prints.
- The installed binary is `openbutler` (`[[bin]] name` — the package
  keeps the repo name), and the default data directory mirrors the
  daemon's `openbutler-daemon` rule: the CLI and the host resolve the
  same directory, and neither touches the legacy Node install's
  `openbutler` data.
- `openbutler usage` no longer reads `state.db`: the numbers live in the
  daemon's `usage.report` (the label, the summary, the day buckets and the
  rendered lines), and the command forwards the flags, prints the lines,
  and — with `--json` — the host's `{label, summary, days}` envelope
  unchanged. The CLI's `rusqlite` dependency went with the query.
- Protocol rev bumped to `2a4a4af` (`SessionRecordWindow`): the windowed
  `session.load` contract (tail window + `entriesFrom`/`totalEntries`,
  older pages through `session.entries-range`) closes the 32 MiB
  transcript gap on the daemon side, so the surface doc's gap list is
  empty.

### Removed

- `openbutler daemon token` and `openbutler daemon pair`. Remote clients
  sign in with the admin account (`openbutler connect <host:port> <username>`)
  and carry the session the host mints; minting and redeeming one-time
  credentials is gone with the token store.

### Fixed

- `run` and the TUI send the `message` object the daemon parses
  (`{text, images}`) instead of flat `text`/`images`, and `run` surfaces a
  refused send as the daemon sentence instead of waiting five seconds and
  reporting "the host did not start the run".

- The TUI grows a reasoning block as it streams. A text continuation was
  applied only to assistant entries, so the host's reasoning deltas were
  dropped and the thinking block stayed empty on screen for the whole run.

- The background host is spawned with this CLI's data directory. The child
  inherited the environment and resolved its own directory, so an overridden
  directory left the CLI probing an endpoint nothing was serving; the spawn
  now sets OPENBUTLER_DATA_DIR the way the foreground path already did.

- The framed client keeps every frame a socket read completes. One read can
  carry a burst (the host writes a response and the events behind it back to
  back); only the first frame was returned, so a run could lose its final
  state patch and wait out the idle deadline. The client now queues every
  frame it decoded.

[Unreleased]: https://github.com/m4rs1l/openbutler-cli/commits/main
