# oncue

**An attention queue for your AI coding agents.**

When you run many Claude Code / Paseo sessions at once, the hard part is not
watching them work. It is noticing which ones stopped and are waiting on *you*.
oncue puts those sessions at the top, most urgent first, and tells you why each
one is waiting.

```
 oncue   Needs you 4 · Working 3 · Dormant 12   22:57:01
┌ Needs you (4) ─────────────────────────────────────────────────────────────┐
│? question  3m  api (feat/threads)   core-api#256  Design the thread feed   │
│! permit    1m  infra                               Add KV scope to token   │
│⏸ limit     4h  app (fix/i18n)                      Fill missing strings    │
│✓ done      2m  chat                 chat#266       Device approval flow    │
└────────────────────────────────────────────────────────────────────────────┘
┌ Working (3) ───────────────────────────────────────────────────────────────┐
│● working       web                                 Landing copy — Bash pnpm│
└────────────────────────────────────────────────────────────────────────────┘
┌ Detail ────────────────────────────────────────────────────────────────────┐
│Design the thread feed                                                      │
│claude · ~/work/api · feat/threads · paseo 6985aed                          │
│Which host should receive cross-service ingest?                             │
│Options: ingest.example.com | api.example.com                               │
└────────────────────────────────────────────────────────────────────────────┘
```

## Why a queue

A process monitor such as [abtop](https://github.com/graykode/abtop) answers
"what is running?". oncue answers "what is waiting on me, and since when?":

| Reason | Meaning |
|---|---|
| `question` | The agent asked a question (`AskUserQuestion`) and is blocked. |
| `permit` | A tool call is waiting for approval. |
| `plan` | A plan is waiting for approval. |
| `merge` | A PR passed checks with nothing open; only your merge decision is left. |
| `error` | The agent stopped with an error. |
| `blocked` | A PR is stuck: failing checks, conflict, requested changes, unresolved threads, or behind base. |
| `limit` | The account hit a usage limit; shows when it resets. |
| `done` | The turn finished and nobody has read the result yet. |
| `idle` | The result was read but not answered. |

A session whose turn ended while its background commands or agents are still
running (polling CI, waiting on a review) stays in the working list with what it
is waiting on: it will resume by itself.

Finished turns that sit longer than `dormant_after_minutes` move to the dormant
list (`d` to show). Questions and approvals never go dormant.

Issue keys (`ENG-123`) are picked up from branches, PR titles, and session
titles. A `⚠` marks live sessions working on the same issue or branch, the
usual way parallel agents collide.

An idle session whose PR needs you moves up to the PR's reason. Your own
recent open PRs with no live session behind them get their own rows.

oncue is read-only. It never writes to agent state and needs no API keys; GitHub
goes through your existing `gh` login, and Linear issue state is added when a
Linear API key is available (see below).

## Install

```bash
cargo install --git https://github.com/ph8nt0m/oncue
```

## Usage

```bash
oncue            # TUI
oncue --once     # print one snapshot and exit
oncue --json     # one JSON snapshot, for scripts
oncue --lang ko  # UI language (en, ko); defaults to LANG
```

Keys: `j`/`k` move, `g`/`G` first/last, `d` toggle dormant, `r` refresh, `q` quit.

## Sources

| Source | What oncue reads |
|---|---|
| Claude Code | `~/.claude/sessions/*.json` for live sessions, the transcript tail for why they wait, `pr-link` entries for PRs. Also `~/.claude-*`, `~/.claude-profiles/*`, `$CLAUDE_CONFIG_DIR`. |
| Paseo | `~/.paseo/agents/**.json` for titles and attention flags, `paseo permit ls` for pending approvals. |
| Linear | Issue title and workflow state for the keys found, with `LINEAR_API_KEY` or `api_key_command`. Without a key, keys and overlap warnings still work. |
| GitHub | `gh api graphql` for the PRs sessions linked and your open PRs updated in the last `stale_after_days`: checks, review decision, conflicts, unresolved threads, merge state. |

## Configuration

`~/.config/oncue/config.toml` (all optional):

```toml
language = "ko"                    # en | ko; empty = LANG
claude_config_dirs = ["~/.claude-work"]
paseo = true
dormant_after_minutes = 360
interval_secs = 2

[github]
enabled = true          # needs `gh auth login`
my_prs = true           # also list your open PRs with no live session
owners = []             # e.g. ["my-org"]; empty = everywhere
stale_after_days = 3
interval_secs = 90

[linear]
enabled = true
api_key_env = "LINEAR_API_KEY"
# Used when the variable is unset; the key stays in memory only.
api_key_command = "security find-generic-password -s oncue-linear -w"
team_keys = []          # e.g. ["ENG"]; empty = your workspace's teams
interval_secs = 120
```

## Roadmap

See [docs/design.md](docs/design.md). Next up: per-account usage limits, jumping
to a session, and notifications.

## License

MIT
