# Contributing

## The gate

Nothing lands unless the whole gate passes on the exact tree that is pushed:

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

CI runs the same sequence on every push (`ci.yml`), and the release workflow
refuses to publish while it is red (`verify` runs before every build job). A
tag is only cut from a tree that passed the gate.

## Review before it leaves

Every change is read again in full against its objective before it is
committed: the diff itself, the tests that pin it, and the real output of the
lane it touches. A change that has not been reviewed this way is not finished.

## Commits and releases

Conventional Commits, one logical change per commit, written about the change
itself rather than about the conversation that produced it. The tag carries the
workspace version; the release refuses to publish when it does not.

The release body is the `CHANGELOG.md` section for that version: run
`scripts/changelog-release.sh <version>` before tagging so the Unreleased
entries move under a version heading. A tag without that section fails the
release instead of publishing an empty body.
