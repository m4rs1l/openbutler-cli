# Changelog

Notable changes to this workspace are recorded here.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

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