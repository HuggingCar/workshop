# Repo Rules

## Scope

- Before edit or verify: collect every `AGENTS.md` from repo root to changed files.
- Rules add together across chain.

## Tooling

- Prefer `rtk uv run …`; no `rtk` installed → plain `uv run …`.
- Never `uvx ruff`: use the pinned dev dep in the workspace venv.

## Verify

Subagents: skip checks below.

Run all commands from the repo root, single uv workspace.

After code edits:

- `rtk uv run ruff format <changed_files>`
- `rtk uv run ruff check <changed_files>`

Before finishing feature work:

- `QT_QPA_PLATFORM=offscreen rtk uv run pytest <target_test_module>`

`QT_QPA_PLATFORM=offscreen` is required for the desktop tests; without it Qt needs a display.
