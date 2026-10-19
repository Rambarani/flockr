<!-- Modified by flockr (https://github.com/Rambarani/flockr): changed from herdr v0.9.5 (Apache-2.0). See NOTICE. -->

# flockr

> flockr is inspired by, and forked from, [herdr](https://github.com/herdrdev/herdr) (Apache-2.0). I really like herdr, I wanted a few extra features, so I cloned it and added them. Thank you to the herdr authors.

<p align="center">
  <img src="assets/logo.png" alt="flockr" width="100" />
</p>

<p align="center">
  English · <a href="README.zh-CN.md">简体中文</a>
</p>

**flockr is the runtime your coding agents live on.** It is a terminal workspace manager (a multiplexer in the tmux family) for AI coding agents such as Claude Code, Codex, Cursor, OpenCode and Grok. It keeps your terminals running in a background server, shows which agent is working, blocked or idle, and adds scheduled jobs, feeds and handoffs on top.

This README is written for two readers: a human setting flockr up for the first time, and an AI agent (for example Claude) that is helping that human. Section 13 is a short rule list written for the agent.

## Contents

1. [What flockr is, and what it adds over herdr](#1-what-flockr-is-and-what-it-adds-over-herdr)
2. [Install and first run](#2-install-and-first-run)
3. [Migrating from herdr, or running side by side](#3-migrating-from-herdr-or-running-side-by-side)
4. [Daily use: keys and mouse](#4-daily-use-keys-and-mouse)
5. [Agents and integrations](#5-agents-and-integrations)
6. [Notification sounds](#6-notification-sounds)
7. [Jobs](#7-jobs)
8. [Feeds](#8-feeds)
9. [Handoff and decisions](#9-handoff-and-decisions)
10. [Worked example: workbox end to end](#10-worked-example-workbox-end-to-end)
11. [Writing jobs with Claude](#11-writing-jobs-with-claude)
12. [Upstream herdr sync](#12-upstream-herdr-sync)
13. [For AI agents](#13-for-ai-agents)
14. [Troubleshooting, development, license](#14-troubleshooting-development-license)

Full reference docs live in [`docs/next/website/src/content/docs/`](docs/next/website/src/content/docs). This README links to them where it summarizes.

---

## 1. What flockr is, and what it adds over herdr

flockr inherits everything herdr does:

- **Detach without stopping work.** A background server owns your terminals. Close the client or lose your SSH connection and the processes keep running. After a server or machine restart the layout is restored and supported agent sessions can resume (the original processes do not survive). See [session state](docs/next/website/src/content/docs/session-state.mdx).
- **One window, several machines.** Local work and saved SSH machines appear in one agent list. See [connecting machines](docs/next/website/src/content/docs/connecting-machines.mdx).
- **Agent status at a glance.** Every pane is marked `working`, `blocked`, `done` or `idle`.
- **Agent-native.** Agents can drive flockr through the CLI and the socket API. See [socket API](docs/next/website/src/content/docs/socket-api.mdx) and [agent skill](docs/next/website/src/content/docs/agent-skill.mdx).
- **Keyboard and mouse are both first-class**, with tmux-style prefix keys (default prefix `ctrl+b`).
- **Plugins.** See [plugins](docs/next/website/src/content/docs/plugins.mdx).
- **One Rust binary**, no Electron.

### What flockr adds

| Feature | What it does | Where to read more |
| --- | --- | --- |
| Agent sidebar grouped by workspace | Agents sit under a header per workspace. Click a header to collapse or expand it. | [configuration](docs/next/website/src/content/docs/configuration.mdx) |
| Rename spaces and agents | Double-click, right-click menu, or keys. | [section 4](#4-daily-use-keys-and-mouse) |
| Notification sounds and a tones picker | Built-in tones, per-agent overrides, a picker in settings. | [section 6](#6-notification-sounds) |
| Which-key popup | Press the prefix and pause: a popup lists every next key. | [keyboard](docs/next/website/src/content/docs/keyboard.mdx) |
| Command palette | Fuzzy search over every action, workspace, tab, pane and agent. `prefix+:`. | [keyboard](docs/next/website/src/content/docs/keyboard.mdx) |
| Agent preview | Recent screen lines of an agent without switching to it. `prefix+i` or hover. | [agents](docs/next/website/src/content/docs/agents.mdx) |
| Agent dashboard | Full-screen table of every agent on every machine. `prefix+d`. | [agents](docs/next/website/src/content/docs/agents.mdx) |
| Jobs | Scheduled commands (every / cron) with a timeout, login-shell environment (reads `~/.zshrc`) and catch-up after downtime. | [section 7](#7-jobs) |
| Feeds | A job publishes a list of items. Shown in a sidebar section and a Feed overlay with categories, a detail pane, actions and sync now. | [section 8](#8-feeds) |
| Handoff and decisions | A job can ask "start an agent on this?". Start opens a Claude tab. Nothing starts without your answer. | [section 9](#9-handoff-and-decisions) |
| System jobs | Jobs built into flockr, such as the upstream herdr watcher. | [section 7](#system-jobs), [section 12](#12-upstream-herdr-sync) |
| Upstream herdr sync | Watches herdr releases and merges them into flockr safely, in an isolated worktree. | [section 12](#12-upstream-herdr-sync) |
| Job authoring kit and `flockr-jobs` skill | `job new`, `validate`, `run --dry-run`, schemas, and a Claude skill that knows the workflow. | [section 11](#11-writing-jobs-with-claude) |
| herdr compatibility aliases | `HERDR_*` variables, `herdr:<id>` hook sources and old config values still work. | [section 3](#3-migrating-from-herdr-or-running-side-by-side) |

### Vocabulary

| Term | Meaning |
| --- | --- |
| Workspace (space) | Project-level container. Owns tabs and panes. |
| Tab | A layout inside a workspace. |
| Pane | A real terminal. |
| Agent | A process flockr recognizes in a pane (Claude Code, Codex, ...). |
| Session | A persistent server namespace. The default `flockr` command uses the default session. |
| Job | A command flockr runs on a schedule. |
| Feed | The list of items a job prints. |
| Decision | A question a job asked you, waiting for Start, Snooze or Dismiss. |
| Prefix | The key that makes the next key go to flockr. Default `ctrl+b`. |

Details: [concepts](docs/next/website/src/content/docs/concepts.mdx).

---

## 2. Install and first run

### Build from source

You need:

- the Rust toolchain. The repo pins it in `rust-toolchain.toml` (channel `1.96.1`, with clippy and rustfmt), so `rustup` picks it up automatically.
- **Zig 0.16.0** on your `PATH`. flockr builds a vendored terminal library (libghostty-vt) with Zig. If a different Zig is first on `PATH`, point the build at the right one with the `ZIG` environment variable, for example `ZIG=/path/to/zig-0.16.0/zig cargo build --release`.

```bash
git clone https://github.com/Rambarani/flockr
cd flockr
cargo build --release

mkdir -p ~/.local/bin
ln -sf "$PWD/target/release/flockr" ~/.local/bin/flockr   # make sure ~/.local/bin is on PATH
flockr --version
```

A symlink means each `cargo build --release` updates the `flockr` you run. Restart the server (below) to use a newly built server.

Release binaries and a package-manager install may also be available from [GitHub releases](https://github.com/Rambarani/flockr/releases) and [`distribution/install.sh`](distribution/install.sh). I have not verified that releases are published for the fork, so building from source is the supported path in this README. Other install notes: [install](docs/next/website/src/content/docs/install.mdx).

### First run

```bash
flockr
```

This launches or attaches to the default background session. If there are no workspaces, flockr opens one. Run an agent in a pane (`claude`, `codex`, ...) and flockr detects it.

| Goal | Command or key |
| --- | --- |
| Detach and leave everything running | `ctrl+b q` (or just close the terminal) |
| Reattach | `flockr` |
| Show status of client and server | `flockr status` |
| Stop the server (this ends all pane processes) | `flockr server stop` |
| Restart the server after rebuilding | `flockr server stop`, then `flockr` |
| Reload `config.toml` in the running server | `flockr server reload-config` |
| Validate `config.toml` | `flockr config check` |
| Named, separate sessions | `flockr session list`, `flockr session attach <name>`, `flockr session stop <name>`, `flockr session delete <name>` |
| Print the default config | `flockr --default-config` |

### Where things live

| What | Location (Linux and macOS) |
| --- | --- |
| Config | `~/.config/flockr/config.toml` (override the path with `FLOCKR_CONFIG_PATH`; `XDG_CONFIG_HOME` is honored) |
| Jobs | `~/.config/flockr/jobs/<id>/job.toml` |
| State (jobs history, feeds, decisions, upstream state) | `~/.local/state/flockr/` (`XDG_STATE_HOME` is honored) |
| Logs | `~/.config/flockr/flockr.log`, `flockr-client.log`, `flockr-server.log` |

Debug builds (`cargo build`, `cargo run`) use `flockr-dev` instead of `flockr` in all of those paths, so a debug build never touches your real session. The state-directory paths come from `src/config/io.rs`; the docs refer to it as `<state dir>`.

---

## 3. Migrating from herdr, or running side by side

flockr uses its own config directory, state directory, socket and log names, so it can run next to herdr without sharing anything.

- `~/.config/herdr` is **never read or migrated**. Copy over what you want by hand (for example keybindings from your old `config.toml`) and run `flockr config check`.
- Panes export the new `FLOCKR_*` variables **and** the legacy `HERDR_*` variables, so tools written for herdr keep working.
- Environment variables read by the server and CLI fall back from `FLOCKR_*` to `HERDR_*` (instance identity variables excepted).
- Agent hook reports with source `herdr:<id>` are accepted as aliases of `flockr:<id>`.
- The old `"herdr"` config and CLI values (`ui.toast.delivery = "herdr"`, `[ui.toast.herdr]`, `--right-click herdr`) are accepted as deprecated aliases. Use the `flockr` spelling in new config.
- Moving a running herdr session is not supported. A herdr session lives in herdr's server. Start flockr, recreate your workspaces, and reinstall agent integrations with `flockr integration install <agent>` (section 5).

The compatibility layer is `src/legacy_herdr.rs`. The list of everything changed from upstream is in [NOTICE](NOTICE).

---

## 4. Daily use: keys and mouse

Press the prefix (default `ctrl+b`), release, then press the key. Pause after the prefix for 400 ms and the **which-key** popup lists every key you can press next. `prefix+?` shows every active binding (press `/` there to filter).

### Keyboard cheat sheet

| Action | Key |
| --- | --- |
| New tab | `prefix+c` |
| Split right / down | `prefix+v` / `prefix+minus` |
| Move between panes | `prefix+h/j/k/l` |
| Workspace navigation | `prefix+w` |
| Detach, leave everything running | `prefix+q` |
| Zoom the focused pane | `prefix+z` |
| Close pane | `prefix+x` |
| Resize mode | `prefix+r` |
| Copy mode | `prefix+[` |
| Next / previous tab | `prefix+n` / `prefix+p` |
| Jump to tab 1 to 9 | `prefix+1..9` |
| Rename tab | `prefix+shift+t` |
| Close tab | `prefix+shift+x` |
| New workspace | `prefix+shift+n` |
| Rename workspace | `prefix+shift+w` |
| Close workspace | `prefix+shift+d` |
| Goto picker (search agents and terminals) | `prefix+g` |
| Toggle sidebar | `prefix+b` |
| **Command palette** | `prefix+:` |
| **Rename the focused agent** | `prefix+shift+a` |
| **Preview the focused agent's screen** | `prefix+i` |
| **Agent dashboard** | `prefix+d` |
| **Jobs overlay (and its Decisions tab)** | `prefix+t` |
| **Feed overlay** | `prefix+f` |
| **Review upstream herdr updates** | `prefix+u` |
| **Collapse / expand the focused agent's workspace group** | `prefix+shift+e` |
| Collapse all / expand all agent groups | unset by default; bind `keys.collapse_agent_groups` and `keys.expand_agent_groups` |

Bold rows are the flockr additions. Defaults come from `src/config/model.rs` and `src/config/keybinds.rs`, and match [keyboard](docs/next/website/src/content/docs/keyboard.mdx). Every binding is configurable, including the prefix:

```toml
[keys]
prefix = "ctrl+a"
```

`prefix+u` is `[upstream] review_key`; set it to `""` to remove it.

### Mouse

| Gesture | Result |
| --- | --- |
| Click a pane, tab, workspace or agent | Focus it |
| Drag a split border | Resize |
| Right-click | Context menu (split, new tab, rename, collapse, feed actions, ...) |
| Double-click a workspace row (name or branch line) | Rename the workspace |
| Double-click a workspace header in the agent list | Rename the workspace |
| Double-click an agent row | Rename the agent |
| Click a workspace header in the agent list | Collapse or expand its group |
| Hover an agent row (600 ms by default) | Agent preview popup |
| Drag-select text | Copy it |
| Double-click a token | Select it and copy |
| Ctrl-click a link | Open it |

Keyboard-only users can set `[ui] mouse_capture = false`, which also turns off the mouse controls above.

### Agent sidebar and settings

- Agents are grouped under a header per workspace (`[ui.sidebar] group_agents_by_workspace = true`). A collapsed header (`▸ workspace`) keeps its state counts, and the name turns the blocked colour if it hides a blocked agent. Collapsed state is remembered across detach and restart.
- The which-key popup is controlled by `[ui.which_key]` (`enabled`, `delay_ms`).
- The agent preview by `[ui.agent_preview]` (`enabled`, `hover_delay_ms`, `lines`).

---

## 5. Agents and integrations

flockr detects supported agents automatically from the foreground process and the screen. Supported: Pi, OMP, Claude Code, Codex, GitHub Copilot CLI, Devin CLI, Droid, Kimi Code CLI, OpenCode, Kilo Code CLI, Hermes Agent, Qoder CLI, Qwen Code, Letta Code, Cursor Agent CLI, MastraCode, Antigravity CLI and Grok CLI. See [agents](docs/next/website/src/content/docs/agents.mdx).

### Status markers

| State | Meaning |
| --- | --- |
| `blocked` | The agent needs input, approval or a decision. |
| `working` | The agent is running. |
| `done` | The agent finished and you have not looked yet. |
| `idle` | Finished or waiting, and seen. |
| `unknown` | flockr cannot tell. |

### Install integrations

An integration lets flockr resume the same agent conversation after a server restart (and, for some agents, report state directly).

```bash
flockr integration install claude      # also: codex, pi, omp, opencode, cursor, grok, ...
flockr integration status              # which are installed, and their versions
flockr integration uninstall claude
```

The full list of agent names is in [integrations](docs/next/website/src/content/docs/integrations.mdx). You can also install them from the **integrations** tab in settings.

### Session restore and resume

| Case | Processes survive | Layout returns | Agent conversation resumes |
| --- | --- | --- | --- |
| Detach and reattach | Yes | Yes | Yes (never stopped) |
| Server restart | No | Yes | Only with an installed integration |

Resume is on by default (`[session] resume_agents_on_restore = true`). Details: [session state](docs/next/website/src/content/docs/session-state.mdx).

---

## 6. Notification sounds

Sounds play in the local client when an agent in a background workspace finishes (`done`) or needs input (`request`).

```toml
[ui.sound]
enabled = true
volume = 1.0            # 0.0 to 1.0
done = "chime"          # "default", a built-in name, or a file path
request = "ping"
# built-in names: chime, ping, pop, bell, blip, chirp

[ui.sound.agents]
droid = "off"           # default | on | off  (droid is muted by default)

[ui.sound.agents.claude]
done = "bell"
request = "chirp"
volume = 0.4
```

- Relative file paths resolve from the config file's directory. mp3 and wav work everywhere.
- **Tones picker:** open settings and go to the **tones** tab. Highlighting a row plays it. `enter` uses it for both notifications, `d` for done only, `r` for request only, `p` replays. It writes only the `done` and `request` keys.
- `FLOCKR_DISABLE_SOUND` disables playback. `flockr config check` reports unplayable files.

Reference: [configuration, Sound](docs/next/website/src/content/docs/configuration.mdx).

---

## 7. Jobs

A job is a command flockr runs for you on a schedule: sync a tracker every hour, check a build each morning. The server runs it in the background with a timeout, records the result and can toast you on failure. Jobs are global per user, not per workspace.

```text
 job.toml ──► scheduler ──► run command (timeout, login-shell env)
                                 │
                 ┌───────────────┼────────────────┐
                 ▼               ▼                ▼
            run record     [feed] command    [handoff] check
          (last 20 runs)   → feed.json       → decision → Start → Claude tab
                           → sidebar/overlay
```

Basics:

- Each job lives in `~/.config/flockr/jobs/<id>/job.toml`. Edits are picked up in about 30 seconds, or at once with `flockr server reload-config`.
- By default a job runs with your **login-shell environment**, so credentials in `~/.zshrc` are available.
- If flockr was not running when a job was due, it runs **once** when the server starts (catch-up).
- A job never overlaps itself: a run due while the last one is still going is recorded as `skipped: still running`.
- `flockr job new` always writes a job **disabled**.

### job.toml reference

```toml
title = "workbox sync"
command = ["workbox", "sync"]      # argv; use ["sh", "-c", "..."] for a shell command
schedule = "0 10 * * 1-5"          # five-field cron, local time  (or: every = "1h")
timeout = "10m"
enabled = true
notify = "on_failure"              # never | on_failure | on_change | always
```

| Key | Meaning |
| --- | --- |
| `id` | Optional. If present it must equal the directory name. Letters, digits, `.`, `_`, `-`; up to 120 characters. |
| `title` | Name shown in lists and toasts. Defaults to the id. |
| `command` | Required argv array. No shell unless you write one. |
| `schedule` or `every` | Required, exactly one of them. |
| `timeout` | Default `10m`, min `1s`, max `24h`. On expiry the command and its children are stopped. |
| `enabled` | Default `true`. |
| `env`, `env_file`, `[env]` | Environment, see below. |
| `cwd` | Working directory. Defaults to the job directory. `~` and relative paths work. |
| `notify` | Toast policy. Default `on_failure`. |
| `[feed]` | Gives the job a feed: `command` (required argv) and `title`. [Section 8](#8-feeds). |
| `[handoff]` | After a run, ask whether to start an agent tab. [Section 9](#9-handoff-and-decisions). |

Unknown keys are errors, so a typo like `evry` is reported rather than ignored. Problems show in `flockr job list` and the Jobs overlay.

### Schedules

- `every = "5m"`: durations like `5m`, `1h`, `1h30m`, `1d`. Minimum `1m`. Next run is one interval after the previous run started.
- `schedule = "0 10 * * 1-5"`: five-field cron in local time (`minute hour day-of-month month day-of-week`). Supports `*`, values, ranges, lists, steps, and `jan` / `mon-fri` names. `@daily`-style macros are not supported.
- Change timing without editing the file: `flockr job schedule <id> every 2h` or `flockr job schedule <id> "0 9 * * 1-5"`, or press `t` in the Jobs overlay.

### Environment modes

| Setting | Behavior |
| --- | --- |
| `env = "login-shell"` (default) | flockr runs `$SHELL -l -i -c` once and uses the resulting variables (so `~/.zshrc` is read). 5 s capture timeout, cached 10 minutes. `flockr job env refresh` recaptures now. If the capture fails, the server environment is used and the run record says why. |
| `env = "server"` | The environment of the flockr server process. |
| `env_file = "secrets.env"` | `KEY=VALUE` lines, relative to the job directory. |
| `[env]` table | Non-secret overrides. When using a table, put the mode inside it: `mode = "server"`. A file cannot hold both `env = "..."` and an `[env]` table. |

Precedence, last wins: base environment, `env_file`, `[env]`, then the variables flockr always sets (`FLOCKR_ENV`, `FLOCKR_SOCKET_PATH`, `FLOCKR_BIN_PATH`, `FLOCKR_JOB_ID`, `FLOCKR_JOB_DIR`, `FLOCKR_JOB_STATE_DIR`). Environment values are kept in memory only, but **job output is stored**, so never print secrets from a job.

### Notifications (`notify`)

| Value | Toast when |
| --- | --- |
| `never` | Never. |
| `on_failure` | The run failed, timed out or could not start (default). |
| `on_change` | Status or stdout differs from the previous run. |
| `always` | Every run. |

### Job CLI

| Command | Purpose |
| --- | --- |
| `flockr job list [--json]` | State, schedule, next run, last result of every job. |
| `flockr job status <id>` | Settings and recent runs. |
| `flockr job run <id> [--wait]` | Run now (works for disabled jobs). `--wait` exits non-zero if the run fails. |
| `flockr job run <id> --dry-run [--no-exec]` | Run once in the foreground, record nothing, show the feed and handoff. **Runs the real command** unless `--no-exec`. |
| `flockr job logs <id> [-n LINES]` | Stdout then stderr of the last run. |
| `flockr job enable <id>` / `disable <id>` | Override `enabled`. Enabling restarts the schedule from now. |
| `flockr job schedule <id> <every 2h or CRON>` | Change timing. |
| `flockr job new <id> [--template shell\|python\|rust\|workbox] [--every I \| --schedule C] [--feed] [--handoff]` | Scaffold a disabled job. |
| `flockr job validate [<id>\|<path>] [--run-feed] [--json]` | Check a job without waiting for its schedule. Exit non-zero on errors. |
| `flockr job build <id>` | Pre-compile a `rust` template job (needs `rust-script`). |
| `flockr job schema [--feed]` | JSON Schema for `job.toml`, or for feed output. |
| `flockr job skill` | Print the Claude authoring skill. |
| `flockr job env refresh` | Recapture the login-shell environment. |

`new`, `validate`, `run --dry-run`, `build`, `schema` and `skill` work without a running server. Templates: `shell` (`run.sh`), `python` (`run.py`), `rust` (`job.rs`, via `rust-script`), `workbox` (copy of the example in section 10).

### Jobs overlay

`prefix+t`, the **jobs** entry in the global menu, or **jobs** in the command palette opens a table of jobs.

| Key | Action |
| --- | --- |
| `↑`/`↓`, `j`/`k`, `g`/`G` | Move |
| `r` | Run the selected job now |
| `l` or `enter` | Show the last run's output |
| `e` | Enable or disable |
| `t` | Change the schedule (`every 2h` or cron; validated as you type) |
| `tab` | Switch to the Decisions list |
| `esc` or `q` | Close |

### System jobs

Jobs built into flockr. Their ids start with `system.`, they appear in the same list with source `system`, and their timing comes from `config.toml`.

| Job | What it does |
| --- | --- |
| `system.upstream-herdr` | Checks upstream herdr releases (once a day by default) and asks whether to copy the updates. See [section 12](#12-upstream-herdr-sync). |

```toml
# config.toml
[jobs.system.upstream-herdr]
every = "24h"          # or schedule = "0 9 * * *"; exactly one
enabled = true
notify = "never"
```

Full reference: [jobs](docs/next/website/src/content/docs/jobs.mdx).

---

## 8. Feeds

A **feed** is a list of items that a job prints, for example "what needs my attention today". flockr stores the latest list, shows it, and lets you run actions on items.

**The contract is generic.** Any program that prints the feed JSON below on stdout can be a feed. workbox is just one adapter that ships with flockr (`flockr feed adapt workbox`); you can write your own in shell, Python or Rust.

Give a job a feed with a `[feed]` table:

```toml
[feed]
command = ["sh", "-c", "workbox list --json | flockr feed adapt workbox --title 'Today (workbox)'"]
title = "Today (workbox)"          # optional; wins over the title the command prints
```

The feed command runs with the same environment, working directory and timeout as the job. It refreshes after every **successful** run of the job, on demand (`flockr feed refresh <job>`, `R` in the overlay, **refresh feed** in the palette), after an item action, and once at server start if the feed was never read. If a refresh fails, flockr **keeps the last good items and marks the feed stale** (`!` in the sidebar header, `stale:` in the overlay).

### Feed format, version 1

The command prints one JSON object on stdout, at most 4 MiB, and **nothing else** on stdout.

```json
{"version": 1, "title": "Today (workbox)",
 "groups": [{"id": "mentions", "title": "JIRA Mentions"}],
 "items": [
  {"id": "jira:NH-1412", "title": "Fix login redirect", "group": "mentions",
   "subtitle": "jira · mention", "state": "unread", "badge": "NH-1412",
   "url": "https://example.atlassian.net/browse/NH-1412",
   "updated_at": "2026-10-18T08:30:00Z",
   "body": "Can you look at the redirect?\nIt blocks the release.",
   "fields": [{"label": "Source", "value": "jira"}, {"label": "Status", "value": "In Progress"}],
   "actions": [
     {"key": "d", "title": "done", "command": ["workbox", "mark", "jira:NH-1412", "done"]}
   ]}
]}
```

| Field | Meaning |
| --- | --- |
| `version` | `1`. Missing is read as 1; any other number is an error. |
| `title` | Feed name. The job's `feed.title` wins, then this, then the job title. |
| `groups` | Optional, up to 16 categories in tab order: `{"id", "title", "short_title"}`. |
| `items[].id` | Required, unique. Duplicates keep the first. |
| `items[].group` | Optional id of a declared group. Missing or undeclared goes to an implicit `other` group. |
| `items[].title` | Row text. Defaults to the id. |
| `items[].subtitle`, `badge` | Secondary text; a short label such as a ticket key. |
| `items[].state` | `unread`, `read` (default), `done`, `warn`, `error`. Drives the dot and the unread count. |
| `items[].url` | `http://` or `https://` only. |
| `items[].updated_at` | RFC 3339 time, shown as an age. |
| `items[].body` | Plain text for the detail pane (cut at 8 KiB). |
| `items[].fields` | Up to 12 `{"label", "value"}` pairs shown as a table in the detail pane. |
| `items[].actions` | Up to 8 actions: `{"key", "title", "command"}`. |

Limits: 500 items; 200 characters for `id` and `title`. Unknown fields are ignored. A bad item or action is dropped with a note (see `flockr feed show`) rather than failing the feed. `flockr job schema --feed` prints the exact JSON Schema.

**Action keys:** a single visible character. Not allowed: named keys (`Enter`, `esc`, arrows, `tab`, `space`), or `j k q g G R S / x X w`. In a feed with groups, `[`, `]` and `0` to `9` are reserved too. Running an action starts its command with the job's environment plus `FLOCKR_FEED_ITEM_ID` and `FLOCKR_FEED_ACTION_KEY` (60 s max), then refreshes the feed. Commands stay on the server; clients send only job, item id and key.

### Adapters

```sh
workbox list --json | flockr feed adapt workbox --title "Today (workbox)"
workbox list --tab today --json | flockr feed adapt workbox     # only the today tab
```

`flockr feed adapt workbox` reads the JSON array from `workbox list --json` on stdin and prints a feed. It sorts items into category tabs: New House PRs, JIRA Mentions, New Tickets, Old House PRs, My PRs, and Other. Each item gets actions `d` (done), `s` (seen) and `t` (today / untoday). `--bin` changes the program the actions run. It needs no server.

### Sidebar section

Below the agent list each feed gets a collapsible section:

```text
 ▾ Today (workbox) 3
  ● NH-1412 Fix login redirect
  ○ NH-1301 Reply to Dana
   +4 more
```

- Click a row to open its link (no link: opens its action menu). Right-click for **Open link**, one entry per action, **Refresh feed**, **Sync now (run job)**, **Open feed**, **Collapse**.
- Click the header to collapse. Click `+N more` to open the Feed overlay.
- A feed with groups shows one compact row per category (`New House PRs 9 ●3`); click it to open the overlay on that category.
- Config: `[ui.sidebar.feeds]` with `enabled = true`, `show = "all"` (or `["workbox-sync"]`), `max_rows = 8`.

### Feed overlay (`prefix+f`)

Master-detail view: list on the left, detail pane on the right (title, state, subtitle, time, link, fields, body, action keys). Narrow terminals stack the pane under the list.

| Key | Action |
| --- | --- |
| `↑`/`↓`, `j`/`k`, `g`/`G` | Move |
| `[` / `]` | Previous / next category tab (feeds with groups) |
| `0`, `1`-`9` | Select All, or the first nine categories |
| `tab`, `shift+tab`, `←`/`→` | Switch feed |
| `ctrl+d`, `ctrl+u` | Scroll the detail pane |
| `space` | Zoom the detail pane and back |
| `enter` | Open the item's link |
| `/` | Filter within the selected category (`enter` keeps, `esc` clears) |
| item action keys | Run that action on the selected item (listed at the bottom of the pane) |
| `R` | Refresh the feed (run the feed command again) |
| `S` | **Sync now**: run the feed's job; the feed refreshes when it succeeds |
| `x` | Select / unselect the current item (a `✓` column; the header shows `N selected`) |
| `X` | Clear the selection |
| `w` | **Work on it**: open a new tab and start an agent (default `claude`) with the selected items, or the current item if none are selected |
| `esc` or `q` | Leave zoom, then clear the selection, then the filter, then close |

#### Work on items in a new agent tab (`x` / `w`)

Select items with `x` (or click the `✓` column, or ctrl/cmd-click a row). Selections survive category tabs and filtering, and are cleared when you switch feeds. Press `w`: flockr renders a prompt from the items, writes it to `<state_dir>/jobs/<id>/work/<time>.md`, opens a focused tab, and starts the agent there. By default the agent starts **without** submitting anything: the prompt is typed into its input box as a draft, so you add your own instructions and press Enter yourself. Up to 50 items per request.

Configure it per job with an optional `[feed.work]` table (all keys optional):

| Key | Default | Meaning |
| --- | --- | --- |
| `agent_command` | `"claude"` | Command to start. `{prompt_file}` is replaced by the prompt file path (then `submit` and `prefill` do not apply: you chose how the agent gets the file). Otherwise see `submit` |
| `submit` | `false` | `false`: start the agent alone, then type the prompt into its input box without Enter. `true`: pass the prompt as the first argument, so the agent runs it at once |
| `prefill` | `"paste"` | With `submit = false`: `"paste"` types the whole prompt as a bracketed paste (Claude Code shows a long paste as `[Pasted text #1 +N lines]`); `"mention"` types one line, `@<prompt file path> ` (Claude's @-file mention), so the draft stays short. Not allowed with `submit = true` |
| `cwd` | the job's `cwd`, else `$HOME` | Tab folder (template). A templated folder that doesn't exist falls back to the default with a warning |
| `tab_title` | `"{{job.title}} · {{count}} items"` | Tab title (template) |
| `prompt` / `prompt_file` | built-in (below) | Prompt template; use one, not both |

Template variables: `{{items}}` (readable list: badge, title, link, fields, body), `{{items_json}}`, `{{count}}`, `{{date}}`, `{{job.id}}`, `{{job.title}}`, `{{feed.title}}`, `{{item.id|title|subtitle|badge|url|state|group|updated_at|body}}` and `{{item.field.<label>}}` for the **first** selected item (labels are lowercased, other characters become `_`, so `Repo` → `{{item.field.repo}}`).

Built-in prompt: *"Here are N items from &lt;feed title&gt; I want to work on: … Read each link/ticket, summarize what each needs, and propose a plan. Ask before changing code."*

Example for workbox (the adapter adds a `Repo` field for GitHub/Bitbucket items; Jira items have none):

```toml
[feed.work]
agent_command = "claude"
cwd = "~/code/work/{{item.field.repo}}"   # falls back if the folder doesn't exist
tab_title = "workbox · {{count}} items"
prompt = """
I picked {{count}} items from my workbox list:

{{items}}

Read each link, say what each needs, propose a plan, and ask before changing code.
"""
```

How the draft is typed: flockr waits for the new pane's agent to be detected and idle (the same status `flockr agent wait` uses) and for the terminal to have turned bracketed paste on, then pastes with no Enter. The wait runs on the server's timer, never blocking it. After 20 seconds it types anyway and logs a warning. For an agent flockr does not recognise there is no detection to wait for, so it waits a fixed 1.5 seconds. If bracketed paste is still off, it types the one-line `@<prompt file>` instead of a multi-line prompt, so a newline cannot submit half of it. The notice reads `opened claude with 3 items as a draft — add your note and press Enter`.

`flockr job validate` checks `[feed.work]` (unknown variables, missing prompt file or agent program, a bad `prefill`), and `flockr job run <id> --dry-run` previews the `w` prompt using the first two feed items, including whether it is submitted or left as a draft. A selection that spans several repos opens in the first item's repo.

### Feed CLI

```sh
flockr feed list [--json]                    # feeds with item and unread counts, staleness
flockr feed show <job> [--json]              # items, their actions and ids, and feed notes
flockr feed refresh <job> [--no-wait]        # run the feed command now
flockr feed action <job> <item> <key>        # run an item's action, then refresh
flockr feed adapt workbox [--title T] [--bin PROGRAM]   # stdin: workbox JSON, stdout: a feed
```

`refresh` and `action` wait and exit non-zero on failure; `--no-wait` returns once started.

---

## 9. Handoff and decisions

A job can end with a question for you. After a run, flockr decides whether the job has something worth your attention. If so it records a **decision**, shows a toast and **waits**. It never starts an agent on its own. You answer **Start**, **Snooze 1h** or **Dismiss**. **Start** opens a tab in the directory you chose and runs your agent with a prompt built from the job output. The agent runs in a real terminal, so you can watch and steer it.

```toml
[handoff]
when = "on_change"                 # on_success | on_change | on_failure | always | "exit:<code>"
ask = "{{count}} items need you. Start Claude?"
agent_command = "claude"           # default; any command
cwd = "~/Desktop/poc/workbox"      # the tab's working directory; default: the job's cwd
tab_title = "workbox · {{date}}"   # default: "{{job.title}} · {{date}}"
prompt_file = "prompt.md"          # a template in the job directory ...
# prompt = "Triage {{count}} items:\n{{feed_json}}"   # ... or inline (not both)
```

| Key | Meaning |
| --- | --- |
| `when` | Which runs ask. `on_change` (default): stdout or the feed differs from the previous run (the first run counts). |
| `ask` | The question (template). |
| `agent_command` | What the tab runs. Default `claude`. If it contains `{prompt_file}` that is replaced by the prompt file path; otherwise the prompt is passed as the first argument (`claude "$(cat '<file>')"`). |
| `submit` | Default `true`: the prompt is passed to the agent, which runs it at once (an upstream sync handoff must run). `false`: the agent starts alone and the prompt is typed into its input box as a draft, without Enter, once the agent is ready. Has no effect when `agent_command` contains `{prompt_file}`. |
| `prefill` | With `submit = false`: `"paste"` (default, the whole prompt as a bracketed paste) or `"mention"` (one line, `@<prompt file>`). |
| `cwd` | Tab working directory. Must exist when you press Start. |
| `tab_title` | Tab name, cut to 60 characters. |
| `prompt_file` or `prompt` | The prompt template. At most one. |

### Template variables

| Name | Value |
| --- | --- |
| `{{stdout}}` | The run's stdout (handoff line removed), cut to 12,000 characters. |
| `{{stderr_tail}}` | End of stderr, at most 4,000 characters. |
| `{{exit_code}}` | Exit code, empty if none. |
| `{{feed_json}}` | The job's feed as `{"title","total","items":[...]}` without action commands. `{"items":[]}` without a feed. |
| `{{count}}` | `context.count` of a dynamic handoff, else the feed's unread count, else 0. |
| `{{date}}` | Today's local date, `YYYY-MM-DD`. |
| `{{job.id}}`, `{{job.title}}`, `{{run.id}}` | The job and the run. |
| `{{context.<path>}}` | A value from a dynamic handoff's `context` object. |

Unknown names render empty and are logged. Substitution is a single pass, so job output cannot inject more placeholders. The rendered prompt is written to `<state dir>/jobs/<id>/handoffs/<run>.md`.

**Dynamic handoff:** a job can decide by itself by printing a stdout line that starts with `{"flockr_handoff":`. The last such line asks regardless of `when`.

### Decisions tab

Open the Jobs overlay (`prefix+t`), then press `tab`. Decisions are stored in `<state dir>/decisions.json` and survive restarts (at most 50 kept). You are told by a toast, a badge on the Jobs title (`jobs  [decisions (2)]`), **decisions (N)** in the command palette, and the CLI.

| Key | Action |
| --- | --- |
| `enter` or `s` | **Start**: open the tab and run the agent |
| `z` | **Snooze** for 1 hour, then it comes back with a new toast |
| `x` | **Dismiss** |
| `p` | Preview the command, directory, tab title and rendered prompt |

If the directory or prompt file is missing, nothing opens, the reason is recorded on the decision, and it stays pending so you can fix the cause and press Start again.

```sh
flockr decision list [--all] [--json]
flockr decision start d3
flockr decision snooze d3 --for 2h
flockr decision dismiss d3
```

---

## 10. Worked example: workbox end to end

[`examples/jobs/workbox-sync/`](examples/jobs/workbox-sync) is an hourly `workbox sync` job with a feed and a handoff. It assumes the `workbox` program is installed and on your `PATH`.

```sh
# 1. Scaffold (writes ~/.config/flockr/jobs/workbox-sync/ with job.toml + prompt.md, disabled)
flockr job new workbox-sync --template workbox
#    or copy by hand:
#    mkdir -p ~/.config/flockr/jobs/workbox-sync
#    cp examples/jobs/workbox-sync/{job.toml,prompt.md} ~/.config/flockr/jobs/workbox-sync/

# 2. Check it (parses the file, resolves the command, checks templates, shows next run times)
flockr job validate workbox-sync --run-feed

# 3. Try it once. This RUNS `workbox sync` for real but records nothing.
flockr job run workbox-sync --dry-run

# 4. Switch it on, sync now, look at the items
flockr job enable workbox-sync
flockr job run workbox-sync --wait
flockr feed show workbox-sync
```

Edit `cwd` in the `[handoff]` table of `job.toml` to the directory where Claude should start (the example uses `~/Desktop/poc/workbox`).

**Run it at 10:00 on weekdays instead of hourly:**

```sh
flockr job schedule workbox-sync "0 10 * * 1-5"
```

(or in the file: remove `every = "1h"` and add `schedule = "0 10 * * 1-5"`; a job has exactly one of the two).

**What you see:**

- A **Today (workbox)** section under the agent list in the sidebar, with the unread count. Click a row to open the item. Right-click for done / seen / today.
- `prefix+f` opens the Feed overlay: category tabs, detail pane, `d` `s` `t` actions, `R` refresh, `S` sync now, `x` select and `w` to start Claude on the selected items.
- When a sync changes something, a toast asks "N items need you. Start Claude?". The question waits in the Decisions tab (`prefix+t`, then `tab`). `enter` opens a tab running `claude` with `prompt.md` filled in. Nothing starts until you press it.
- If the feed command fails (for example offline), the last list stays and the sidebar header shows `!`.

More variants (today tab only, ask on every sync, hide the section, different agent) are in the example's [README](examples/jobs/workbox-sync/README.md).

---

## 11. Writing jobs with Claude

flockr ships a skill, `flockr-jobs` ([`skills/flockr-jobs/SKILL.md`](skills/flockr-jobs/SKILL.md)), that teaches Claude the job and feed contract. Install the copy that matches your binary:

```sh
mkdir -p ~/.claude/skills/flockr-jobs
flockr job skill > ~/.claude/skills/flockr-jobs/SKILL.md
```

flockr does not install it for you. For agents without a skill system, paste the output of `flockr job skill` into the project or user instructions.

Then ask in plain words: "check my open PR count every 30 minutes and tell me when it changes". The agent follows this loop:

```sh
flockr job new <id> --template shell --every 30m --feed --handoff   # 1. scaffold (disabled)
# 2. edit the files
flockr job validate <id> --run-feed                                 # 3. check
flockr job run <id> --dry-run                                       # 4. try once, records nothing
flockr job enable <id>                                              # 5. only after you say yes
```

Safety rules:

- Jobs always start **disabled**. The agent must not enable one without asking you.
- `--dry-run` runs the real command. A job that changes data changes it.
- Never print secrets from a job: stdout and stderr are stored (last 16 KiB per run).
- Feed stdout must be only the JSON object. Send logs to stderr.

The separate `flockr` skill (`skills/flockr/SKILL.md`, install with `npx skills add Rambarani/flockr --skill flockr -g`) teaches agents to control panes and workspaces.

---

## 12. Upstream herdr sync

flockr tracks upstream herdr so you can pull in its new releases safely. This is the one place where "herdr" is still the live name, because it is about the upstream project.

How it works:

1. The system job `system.upstream-herdr` checks the herdr releases (GitHub API for `herdrdev/herdr`) once a day. It fails quietly when offline.
2. When a release is newer than the synced base (recorded in `.flockr/upstream-base`), it raises a **decision**: a toast like `herdr v0.9.7 is out (3 features, 5 fixes). Copy updates?`.
3. `prefix+u` (or `flockr upstream review`) opens a review popup listing pending releases with notes and the choices **Copy updates**, **Skip this version**, **Later**.
4. **Copy updates** opens a new flockr tab running `flockr upstream sync <tag>`. The sync never touches your checkout or the `flockr` branch: it adds a git remote `upstream` if missing, fetches tags, creates a worktree at `<repo_path>/../flockr-sync-<tag>` on a new branch `upstream-sync/<tag>`, and starts your agent with a prompt. The agent merges the tag, keeps flockr's rename, `HERDR_*` aliases, hosting URLs and features, and runs build, tests, clippy and fmt. When all pass it records the verified commit.
5. Nothing reaches `flockr` until you run `flockr upstream apply <tag>`. It checks the sync branch is verified and your checkout is clean and on `flockr`, asks for confirmation (`--yes` skips it), merges, runs `cargo build --release`, removes the worktree and records the new base.

```toml
[upstream]
enabled = true
include_prereleases = false
repo_path = "~/src/flockr"         # the flockr source checkout
agent_command = "claude"
review_key = "prefix+u"
```

| Command | Purpose |
| --- | --- |
| `flockr upstream check [--json\|--job]` | Query GitHub now. |
| `flockr upstream status [--json]` | Synced base, last check, pending and skipped releases. |
| `flockr upstream review` | The review popup. |
| `flockr upstream copy [TAG]` | Open the sync tab (default: latest pending). |
| `flockr upstream skip TAG` | Stop notifying about a release. |
| `flockr upstream sync TAG` | Worktree plus agent that merges the release. |
| `flockr upstream apply TAG [--yes]` | Merge a verified sync branch into flockr and rebuild. |

Debug builds do not run the watcher on a schedule; `flockr job run system.upstream-herdr` runs it by hand. Timing is set under `[jobs.system.upstream-herdr]`. Reference: [configuration, upstream herdr watcher](docs/next/website/src/content/docs/configuration.mdx).

---

## 13. For AI agents

You are Claude (or another agent) reading this to help the user with flockr. Follow these rules.

1. **Detect whether you are inside flockr.** If the environment variable `FLOCKR_ENV` is `1`, you are running in a flockr-managed pane and may use the `flockr` CLI against the local socket. If it is not set, do not assume a flockr server is reachable; say so.
2. **`flockr --help` is the authority** for commands on the user's installed version, then `flockr <group> --help` (`job`, `feed`, `decision`, `upstream`, `integration`, `session`, `config`). Leaf commands print the top-level help; for flags, use this README and the docs below. If this README and `--help` disagree, trust `--help` and tell the user.
3. **Never stop or restart the user's server without asking.** `flockr server stop` ends every pane process and every running agent, including possibly you. Prefer `flockr server reload-config` for config changes.
4. **Writing jobs:** use `flockr job new`, then `flockr job validate <id> --run-feed`, then `flockr job run <id> --dry-run`. Do not run `flockr job enable` until the user says yes. Remember `--dry-run` runs the real command; use `--no-exec` when the command has side effects.
5. **Never print secrets** in job output, feed output or logs. Output is stored.
6. **Feed commands:** stdout must be one JSON object and nothing else. Action `key` is one character and not one of the reserved ones in section 8.
7. **Do not touch `~/.config/flockr` or the user's running session while testing a build.** A debug build (`cargo run`) uses `flockr-dev` paths; clear inherited socket variables first: `env -u FLOCKR_SOCKET_PATH -u FLOCKR_CLIENT_SOCKET_PATH cargo run -- <command>`.
8. **Where docs live:** `docs/next/website/src/content/docs/*.mdx` (jobs, keyboard, configuration, agents, concepts, session-state, socket-api, plugins, cli-reference). Schemas: `docs/next/api/flockr-job.schema.json`, `flockr-feed.schema.json`, `flockr-api.schema.json`. Repo rules for contributors: `AGENTS.md` and `CONTRIBUTING.md`.
9. **Feature work rules:** do not edit `skills/flockr/SKILL.md` (it tracks the latest stable release), root `CHANGELOG.md` or `docs/next/CHANGELOG.md` in normal feature or fix work. Put user-facing docs under `docs/next/website/src/content/docs/`.
10. **Naming:** the product is flockr. Use "herdr" only for the upstream project, the `system.upstream-herdr` job, legacy `HERDR_*` compatibility, and migration.
11. **Say what you did not verify.** If you could not run a command or confirm a behaviour, say so instead of guessing.

---

## 14. Troubleshooting, development, license

### Troubleshooting

| Problem | What to do |
| --- | --- |
| Something is off | `flockr -V` and `flockr status` show the client and server versions and state. |
| `flockr: command not found` | Check the symlink in `~/.local/bin` and that it is on `PATH`. Restart the terminal. |
| Rebuilt, but the old behavior remains | The old server is still running. `flockr server stop` (ends panes), then `flockr`. |
| Logs | `~/.config/flockr/flockr.log`, `flockr-client.log`, `flockr-server.log` (`flockr-dev` for debug builds). `FLOCKR_LOG=flockr=debug` for more detail. |
| Config looks ignored | `flockr config check` prints diagnostics. `flockr server reload-config` applies edits. |
| Job did not run or failed | `flockr job list` (invalid files show the key and reason), `flockr job status <id>`, `flockr job logs <id>`, or `l` in the Jobs overlay. |
| Job cannot find a tool or credential | The login-shell capture may be stale or failed. `flockr job env refresh`, then `flockr job validate <id>` (it shows the environment mode and whether the command resolves). Check the run record for a "fell back to server environment" note. |
| Feed shows `!` or `stale:` | The feed command failed. The last good items are kept. `flockr feed show <job>` lists notes, `flockr feed refresh <job>` retries. |
| A question never appeared | `flockr decision list --all`. Identical questions are deduplicated. |
| Agent not resuming after restart | `flockr integration status`, then `flockr integration install <agent>` again. |
| A direct keybinding does nothing | Your terminal or OS consumed the chord. See [keyboard](docs/next/website/src/content/docs/keyboard.mdx#going-prefix-free). |

More: [troubleshooting](docs/next/website/src/content/docs/troubleshooting.mdx).

### Development

```bash
git clone https://github.com/Rambarani/flockr
cd flockr
cargo build --release

just test        # cargo nextest + maintenance script tests (needs cargo-nextest)
just check       # formatting, tests, and maintenance checks
```

- Install the runner once: `cargo install cargo-nextest --locked`. Use `just` recipes rather than calling cargo directly for tests.
- Zig 0.16.0 is required for the vendored terminal library; set the `ZIG` environment variable if another Zig is first on `PATH`.
- Debug builds use `flockr-dev` paths, separate from your real flockr.
- AI agents working on the repo: read [`AGENTS.md`](AGENTS.md) first and [`CONTRIBUTING.md`](CONTRIBUTING.md) before opening issues or PRs.

### Thanks

flockr exists because of [herdr](https://github.com/herdrdev/herdr). Every past sponsor and backer of upstream herdr is listed in [SPONSORS.md](SPONSORS.md).

### License and attribution

flockr is licensed under the [Apache License 2.0](LICENSE) (unchanged from upstream). It is a derivative work of herdr, forked at v0.9.5. Modifications are listed in [NOTICE](NOTICE). Upstream copyright and license notices in source files, vendored code and the changelog history are retained.
