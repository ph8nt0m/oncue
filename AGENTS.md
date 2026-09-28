# oncue

Attention queue TUI for AI coding agents (Rust, ratatui). Read
[docs/design.md](docs/design.md) before changing how sessions are classified.

## Language

Everything in the repository is English: code, comments, docs, commit messages,
PRs, issues. User-facing TUI strings live in `src/i18n.rs` with an English and a
Korean entry for each.

## Layout

```
src/
├── main.rs              # CLI flags, TUI loop, --once text output
├── model.rs             # Session, State, Attention, Snapshot (queue order)
├── config.rs            # ~/.config/oncue/config.toml
├── i18n.rs              # en/ko strings, age formatting
├── git.rs               # branch from HEAD without spawning git
├── ui.rs                # ratatui drawing and selection
└── collector/
    ├── mod.rs           # runs collectors, merges, applies dormancy
    ├── claude.rs        # Claude Code sessions + transcript tail
    └── paseo.rs         # Paseo agents + pending permits
```

## Rules

- oncue is read-only toward agents. Never write to `~/.claude*`, `~/.paseo`, or
  any other agent directory. Settings changes (for example a hook installer) must
  be an explicit subcommand the user runs.
- No network access by default. GitHub and Linear go through the user's `gh` and
  API-key setup, opt-in via config.
- Tests use synthetic fixtures only. Never commit real transcripts, session
  files, paths, or names from a real machine.
- A new `Attention` variant needs: its place in the enum order (urgency), an
  i18n label in both languages, an icon and color in `ui.rs`, a row in the
  design doc table, and a test for the detector.
- External CLIs (`paseo`, `gh`) run with a timeout and degrade to a warning in
  the footer, never an error exit.

## Verify

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo run -- --once   # against the local machine
```

CI runs the first three on macOS and Linux.
