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
