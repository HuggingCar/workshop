# Repo Rules

## Scope

- Before edit or verify: collect every `AGENTS.md` from repo root to changed files.
- Rules add together across chain.

## Tooling

- Prefix shell commands with `rtk` when available.
- Use the root Cargo workspace and the pinned Rust toolchain.

## Verify

Subagents: skip checks below.

Run all commands from the repository root.

- `rtk cargo +nightly-2026-03-05 fmt --all --check` (the import grouping settings require nightly rustfmt).
- `rtk cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `rtk cargo test --workspace --all-features`

Use the Rust-native simulator for printer tests; never issue real fiscal operations during checks.
After UI changes, run the actual application and check the affected controls.
