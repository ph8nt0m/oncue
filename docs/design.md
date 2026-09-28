# oncue design

## Problem

One person running many agent sessions in parallel spends most of their input
on coordination, not instructions: "go on", "approved, merge it", "is it done
yet?", "what is left?". Every one of those is a session that stopped and waited
without the owner noticing. Process monitors show what is running; nothing shows
what is blocked on the human.

oncue is that view: a queue of sessions that need the user, ordered by urgency
and wait time, with enough detail to act without opening the session.

## Principles

- **Attention first.** The top of the screen is always "needs you". Working
  sessions are secondary; dormant ones are hidden by default.
- **Say why.** Every queued item has a reason (question, permit, plan, error,
  limit, done, idle) and the text needed to act on it.
- **Read-only and local.** oncue reads agent state from disk and local CLIs. It
  never writes to agent directories. Network sources (GitHub, Linear) are opt-in
  and use the user's existing CLI auth.
- **Cheap to run.** One refresh should stay well under a second with dozens of
  sessions; transcripts are read from the tail, never parsed whole.

## Attention model

`State` is `Working`, `NeedsYou(Attention)`, or `Dormant`. `Attention` is ordered
by urgency, and the queue sorts by that order, then by the longest wait.

| Attention | Detected from |
|---|---|
| Question | Last unanswered `tool_use` is `AskUserQuestion`; or a Paseo permit for it. |
| Permission | A pending Paseo permit for any other tool. |
| Plan | Last unanswered `tool_use` is `ExitPlanMode`. |
| Error | Paseo `attentionReason = error`; or the last assistant entry is an API error. |
| Limited | The last assistant entry is an API error with `error = rate_limit`; `quotaLimits.resetsAt` gives the reset. |
| Unread | Session idle after a normal reply, and Paseo (if present) still flags it. |
| Idle | Session idle and the user already opened it (Paseo cleared the flag). |

`since_ms` is when the wait began: the transcript mtime for questions and plans
(nothing is written while they are open), otherwise the session's
`statusUpdatedAt`, or Paseo's `attentionTimestamp`.

Unread/Idle items older than `dormant_after_minutes` become `Dormant`. Blocking
reasons never do.

## Sources

### Claude Code

- Roots: `~/.claude`, `~/.claude-*`, `~/.claude-profiles/*`, `$CLAUDE_CONFIG_DIR`,
  and configured extras; a root counts if it has `sessions/`.
- Live sessions: `<root>/sessions/<pid>.json` (`sessionId`, `cwd`, `status`
  busy/idle, `statusUpdatedAt`), filtered by `kill(pid, 0)`.
- Transcript: `<root>/projects/<slug(cwd)>/<sessionId>.jsonl`, where the slug
  replaces every non-alphanumeric character with `-`. The last 512 KiB are parsed
  for pending tool uses, the last assistant text, API errors, and `pr-link`
  entries.
- Title: a user-set session name, else the handoff source from a Paseo
  `<chat-history-summary>`, else the first non-trivial prompt.

### Paseo

- `~/.paseo/agents/<workspace>/<id>.json`: title, `lastStatus`,
  `requiresAttention`, `attentionReason`, `persistence.sessionId`.
- `paseo permit ls --json` (4 s timeout): pending approvals.
- An agent whose `persistence.sessionId` matches a live Claude session enriches
  that row; any other active agent (Codex, other providers, closed sessions)
  becomes its own row.

## Roadmap

1. **Local queue** (v0.1): Claude Code + Paseo, TUI, `--once`, `--json`, en/ko.
2. **GitHub**: status of the PRs sessions opened (checks, review decision,
   mergeable, unresolved threads) via `gh`. New queue reasons: `ready to merge`,
   `checks failed`, `changes requested`. Also the user's open PRs that no session
   owns.
3. **Linear**: issue keys from branch names, PR titles, and prompts (configurable
   pattern such as `[A-Z]+-\d+`); issue state next to each session; warn when two
   live sessions work on the same issue or branch.
4. **Usage limits per account**: 5-hour and weekly usage per Claude config root
   and Codex profile, shown in the header, with the reset time. The source needs
   a design decision: a statusline hook (no credentials, only updates while a
   session renders) or the usage endpoint behind an explicit opt-in.
5. **Act from the queue**: jump to the session (tmux, iTerm2, Paseo), send a
   quick reply through `paseo send`, desktop notifications when the queue grows,
   Codex CLI and OpenCode collectors.
6. **Release**: cargo-dist binaries, Homebrew tap, crates.io, a `--demo` mode with
   synthetic data for screenshots.
