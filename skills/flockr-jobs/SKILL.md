---
name: flockr-jobs
description: "Create, edit, validate and debug Flockr jobs: scheduled commands, job feeds and handoffs (a job asking the user to start an agent tab). Use when the user wants a scheduled task, cron job, recurring check, sync or watcher that runs inside Flockr, a feed of items in the Flockr sidebar, a handoff or decision that opens Claude in a new tab, or asks to fix or change a job.toml. Needs the flockr CLI; no running Flockr session is required to author and test a job."
---

# Flockr jobs

A job is a command that the Flockr server runs on a schedule, in the background, with a timeout. The result is recorded and shown in the Jobs overlay (`prefix+t`). A job can also publish a **feed** (a list of items shown in the sidebar and the Feed overlay, with per-item actions) and a **handoff** (after a run, ask the user whether to start an agent in a new tab). Jobs are global per user.

This skill is for writing jobs. The `flockr` CLI is the authority for exact syntax: `flockr job --help`. Print this skill with `flockr job skill`, and the JSON Schemas with `flockr job schema` (job.toml) and `flockr job schema --feed`.

## Rules

- Create jobs with `flockr job new`, never by hand-making the directory. It writes a commented `job.toml`, a script and, optionally, a feed stub and a prompt, and it refuses to overwrite an existing job.
- Jobs start **disabled**. Do not enable one until `flockr job validate` is clean, a dry run looks right, and the user agrees.
- `flockr job run <id> --dry-run` really executes the job's command (and feed command) once. Only Flockr's own bookkeeping is skipped. If the command changes data, sends messages or costs money, use `--no-exec` or test against a stub, and say so to the user.
- Never print secrets from a job. Stdout and stderr are stored in the run record (the last 16 KiB of each), can appear in toasts, and are pasted into handoff prompts. Read credentials from the environment and never echo them.
- Edit only the job directory you were asked to work on: `~/.config/flockr/jobs/<id>/` (debug builds use `flockr-dev` instead of `flockr`; `XDG_CONFIG_HOME` moves it).

## Workflow

1. **Create.** `flockr job new <id> --template shell|python|rust|workbox [--every 1h | --schedule "0 9 * * 1-5"] [--feed] [--handoff]`
   - `shell`: `run.sh`, run as `sh run.sh`. `python`: `run.py`, run as `python3 run.py`. `rust`: a single-file `job.rs`, run with `rust-script job.rs` (install once with `cargo install rust-script`; `flockr job build <id>` pre-compiles it).
   - `workbox`: the tracked workbox-sync example, hourly, with a feed and a handoff.
   - `--feed` adds a feed stub (`feed.sh`, `feed.py` or `feed.rs`) and a `[feed]` table. `--handoff` adds a `[handoff]` table and `prompt.md`.
2. **Edit** the script and `job.toml`. The comments in the generated `job.toml` explain every key.
3. **Validate.** `flockr job validate <id> [--run-feed] [--json]` (or a path to a job directory). It parses `job.toml` with the same parser the server uses, checks that the command resolves on the job's PATH, that scripts and `prompt_file` exist, that templates use only known variables, flags `{prompt_file}` with no prompt, shows the next three run times, and with `--run-feed` runs the feed command and checks its output. It exits non-zero on errors. With no id it checks every job. Fix every `error:` and read every `warning:`.
4. **Dry run.** `flockr job run <id> --dry-run [--no-exec]`. It runs the command and the feed in the foreground with the job's real environment rules, then prints stdout, stderr, exit code, duration, the parsed feed summary and diagnostics, and the handoff exactly as it would be offered: ask text, tab title, cwd, agent command and rendered prompt. Nothing is recorded, no toast, no decision. `--no-exec` renders the handoff for a successful run with empty stdout without running anything.
5. **Enable.** `flockr job enable <id>`, or press `e` on the job in the Jobs overlay (`prefix+t`). Enabling starts the schedule from now. `flockr job run <id> --wait` runs it on the server immediately and exits non-zero if it fails.
6. **Watch.** `flockr job list`, `flockr job status <id>`, `flockr job logs <id> [-n LINES]`, `flockr feed show <id>`, `flockr decision list`.

Edits to `job.toml` are picked up within about 30 seconds, or at once with `flockr server reload-config`.

## job.toml reference

Unknown keys are errors. All problems are reported together.

```toml
id = "my-job"                 # optional; must equal the directory name
title = "My job"              # shown in lists and toasts; default: the id
command = ["sh", "run.sh"]    # required argv; no shell unless you write ["sh", "-c", "..."]
every = "1h"                  # exactly one of every / schedule
# schedule = "0 10 * * 1-5"   # 5-field cron, local time
timeout = "10m"               # default 10m, min 1s, max 24h
enabled = false               # `job new` writes false
notify = "on_failure"         # never | on_failure | on_change | always
cwd = "."                     # default: the job directory; ~ and relative paths work
# env = "server"              # default "login-shell"; with an [env] table use `mode` inside it
env_file = "secrets.env"      # KEY=VALUE lines, relative to the job directory

[feed]                        # optional
command = ["sh", "feed.sh"]
title = "My feed"

[feed.work]                   # optional: what `w` does in the Feed overlay
agent_command = "claude"      # default
cwd = "~/projects/{{item.field.repo}}"   # default: the job's cwd, else $HOME
tab_title = "{{job.title}} · {{count}} items"   # default
# submit = false              # default: the prompt is typed into the agent as an unsent draft; true runs it at once
# prefill = "paste"           # with submit = false: "paste" (default) or "mention" (one line, @<prompt file>)
# prompt = "..." or prompt_file = "work.md"   # default: a built-in prompt listing the items

[handoff]                     # optional
when = "on_change"
ask = "{{count}} new. Start Claude?"
agent_command = "claude"
cwd = "~/projects/app"
tab_title = "my-job {{date}}"
prompt_file = "prompt.md"

[env]                         # non-secret overrides; keep this table last
LOG_LEVEL = "debug"
```

- **Schedule.** `every` is `30s`-style (`5m`, `1h`, `1h30m`, `1d`), minimum `1m`. `schedule` is five cron fields (`minute hour day-of-month month day-of-week`) in local time; names (`mon-fri`) work, `@daily` macros do not. If flockr was not running when a run was due, the job runs once at server start.
- **Overlap and timeout.** A job never overlaps itself: a run that is still going when the next is due makes the next one `skipped: still running`. When `timeout` expires the command and its process group are stopped. Keep `timeout` shorter than `every`.
- **Environment.** `env = "login-shell"` (default) runs `$SHELL -l -i -c` once and gives jobs that environment, so what `~/.zshrc` exports (tokens, `PATH`) is available. The capture has a 5 second timeout and is cached for 10 minutes (`flockr job env refresh` recaptures). If it fails, the run uses the server environment and the run record says so. `env = "server"` uses the Flockr server's environment. A file cannot hold both `env = "..."` and an `[env]` table; with the table, write `mode = "server"` inside it. Precedence, last wins: base environment, `env_file`, `[env]`, then Flockr's own `FLOCKR_ENV`, `FLOCKR_SOCKET_PATH`, `FLOCKR_BIN_PATH`, `FLOCKR_JOB_ID`, `FLOCKR_JOB_DIR`, `FLOCKR_JOB_STATE_DIR` (a directory the job may keep files in). Values are never written to disk or logs by Flockr, but your job's output is.
- **State.** The last 20 runs (start, duration, exit code, output tails) are kept at `<state dir>/jobs/<id>/state.json`.
- **System jobs.** Built-in jobs have ids starting with `system.` (today: `system.upstream-herdr`). They have no `job.toml`. Tune them in `config.toml` under `[jobs.system.<name>]` with `every` or `schedule` (not both), `enabled` and `notify`, or change the timing with `flockr job schedule <id> every 2h` (or a quoted cron expression), which also works for user jobs. `flockr job schema` describes that section.

## Feed contract (version 1)

The feed command's stdout must be **only one JSON object**: no log lines, no banner, no trailing text. Write diagnostics to stderr. It runs after every successful run of the job, on demand (`flockr feed refresh <id>`), after an item action, and once at server start if the feed was never read.

```json
{"version": 1, "title": "Open PRs", "items": [
  {"id": "pr-42", "title": "Fix login redirect", "subtitle": "acme/web", "state": "unread",
   "badge": "#42", "url": "https://github.com/acme/web/pull/42", "updated_at": "2026-10-18T08:30:00Z",
   "actions": [{"key": "d", "title": "done", "command": ["gh", "pr", "close", "42"]}]}
]}
```

- `version` is `1` (missing is read as 1 with a note; any other value is an error). `items` is required.
- `items[].id` is required and unique (duplicates keep the first). `state` is `unread`, `read` (default), `done`, `warn` or `error` and drives the dot and the unread count; an unknown state becomes `read`. `url` must start with `http://` or `https://`.
- **Caps.** At most 500 items and 4 MiB of output. `id` and `title` 200 characters, `subtitle` 300, `badge` 40, `url` 2048, `updated_at` 40, feed `title` 120. Longer text is cut, control characters are removed, line breaks become spaces. A bad item or action is dropped with a diagnostic instead of failing the feed.
- **Actions.** At most 8 per item. `key` is exactly one visible character, unique per item, and not one of `j k q g G R S / x X w W` (the overlay's own keys; `x`, `X`, `w` and `W` select and work) or a named key (`enter`, `esc`, arrows, `tab`, `space`, and the like). `command` is a non-empty argv array (at most 64 arguments of 4096 characters). It runs with the job's environment plus `FLOCKR_FEED_ITEM_ID` and `FLOCKR_FEED_ACTION_KEY`, for at most 60 seconds, then the feed refreshes.
- A feed that fails to refresh keeps its last good items and is marked stale. `flockr feed show <id>` lists the diagnostics.
- `flockr feed adapt workbox` turns `workbox list --json` into a feed; use it as the last stage of a pipeline in a `sh -c` feed command.

## Work on feed items (`[feed.work]`)

In the Feed overlay (`prefix+f`) the user marks items with `x` (`X` clears, ctrl-click also marks) and presses `w`: Flockr opens a tab and starts an agent with the marked items as the prompt (the current item when none is marked). `W` does it for every item of the list as shown (the selected category tab and filter, at most 50). A feed can put an item in several category tabs with `also_groups` (up to 4 extra declared group ids); the workbox adapter uses it for a first Today tab. It is the same start path as a handoff decision, driven by the `feed.work` API method. Every `[feed.work]` key is optional: `agent_command` (default `claude`; `{prompt_file}` as in a handoff), `submit` (default `false`: the agent starts with no prompt and the prompt is typed into its input box as a draft, no Enter, so the user adds a note and presses Enter; `true` passes it as the first argument and the agent runs it at once), `prefill` (with `submit = false`: `"paste"` default, the whole prompt as a bracketed paste, or `"mention"`, one line `@<prompt file> `; not allowed with `submit = true`), `cwd` (a template; default the job's `cwd` when set, else `$HOME`; a missing directory falls back to that default), `tab_title` (default `{{job.title}} · {{count}} items`), and a `prompt` or `prompt_file` template (not both). The default prompt lists the items and asks the agent to read each link, summarize what each needs, propose a plan and ask before changing code.

- **Template variables:** `{{items}}` (readable list: number, badge, title, link, id, state, fields, body; bodies cut to 2,000 characters, the list to 60,000), `{{items_json}}` (48,000), `{{count}}` (at most 50 items are used), `{{date}}`, `{{job.id}}`, `{{job.title}}`, `{{feed.title}}`, `{{item.id}}`, `{{item.title}}`, `{{item.subtitle}}`, `{{item.badge}}`, `{{item.url}}`, `{{item.state}}`, `{{item.group}}`, `{{item.updated_at}}`, `{{item.body}}` (the first marked item) and `{{item.field.<label>}}` (a field of the first item, label in lower case with other characters as `_`, so a `Repo` field is `{{item.field.repo}}`). Substitution is a single pass, as in a handoff; unknown names are validation errors.
- The rendered prompt is written to `<state dir>/jobs/<id>/work/<unix ms>.md`. `flockr job validate <id>` checks the templates; keys for actions must avoid `x`, `X`, `w` and `W`.
- `flockr feed adapt workbox` adds a `Repo` field (the repository name) to GitHub and Bitbucket items whose `where` is `owner/repo#123`, so `cwd = "~/code/work/{{item.field.repo}}"` can open the tab in the local checkout.

## Handoff: ask, then start an agent

After a run (and after the feed refresh, so the feed is current), Flockr decides whether to ask. If so it records a **decision**, shows a toast and a badge in the Jobs overlay, and waits. Nothing starts until the user answers Start, Snooze 1h or Dismiss in the Decisions tab (`prefix+t`, then `tab`) or with `flockr decision start|snooze|dismiss <id>`. Start opens a tab in `cwd` titled `tab_title` and runs `agent_command`.

- **`when`**: `on_success`, `on_change` (default; stdout or the feed differs from the previous run, the first run counts as changed), `on_failure`, `always`, or `"exit:<code>"`.
- **`ask`**, **`tab_title`**, **`cwd`**, **`agent_command`**, **`prompt`** and the prompt file are templates. Default ask: `<title> has something for you. Start an agent?`. Default agent: `claude`.
- **Prompt delivery.** The rendered prompt is written to `<state dir>/jobs/<id>/handoffs/<run>.md`. If `agent_command` contains `{prompt_file}` it is replaced by the quoted path; otherwise the text is passed as the first argument (`claude "$(cat <file>)"`). Set `prompt_file` (a file in the job directory) or an inline `prompt`, not both. `{prompt_file}` with no prompt is an error.
- **`submit` / `prefill`.** `submit` defaults to `true` for a handoff (the agent runs the prompt at once, which a sync handoff needs). `submit = false` starts the agent alone and types the prompt into its input box as an unsent draft once the agent is detected idle (a known agent; 20 s timeout, then it types anyway) or after a fixed 1.5 s (an unknown agent); `prefill = "mention"` types just `@<prompt file> `. A command containing `{prompt_file}` ignores both: validate warns.
- **Template variables:** `{{stdout}}` (last output, 12,000 characters, without the handoff line), `{{stderr_tail}}` (4,000), `{{exit_code}}`, `{{feed_json}}` (the feed without action commands, 48,000), `{{count}}`, `{{date}}` (local `YYYY-MM-DD`), `{{job.id}}`, `{{job.title}}`, `{{run.id}}` and `{{context.<path>}}`.
- **`{{count}}`** is `context.count` of a dynamic handoff if it has one, otherwise the feed's unread count, otherwise 0. Without a `[feed]`, `{{count}}` is 0 and `{{feed_json}}` is `{"items":[]}`.
- **Substitution is a single pass.** A value is never searched for placeholders again, so job output cannot inject more. A name with no value renders empty and shows as a warning in the dry run; `validate` reports unknown names as errors.
- **Dynamic handoff.** If stdout contains a line starting with `{"flockr_handoff":`, the last such line is a handoff that **always asks**, whatever `when` says, even with no `[handoff]` table. Its `ask`, `agent_command`, `cwd`, `tab_title`, `prompt`, `prompt_file` override the table's; `context` is any JSON object for `{{context.*}}`.
  `{"flockr_handoff": {"ask": "Build {{context.id}} failed. Start Claude?", "prompt": "Look at {{context.url}}", "context": {"id": "812", "url": "https://ci.example.com/812"}}}`
- The same decision content is not asked twice, so a job that keeps printing the same thing asks once.

## Debugging

| Symptom | Look at |
| --- | --- |
| Job shows `invalid` in `flockr job list` | `flockr job validate <id>`; the first diagnostic is also in the list |
| Run failed, timed out or did not start | `flockr job logs <id>` (stdout, then stderr, of the last run); `flockr job status <id>` for recent runs and the environment note |
| Command not found only on schedule | The job's PATH is the login shell's. `flockr job validate` says which environment it used; `flockr job env refresh` after editing `~/.zshrc` |
| Feed is empty or marked stale (`!`) | `flockr feed show <id>` for the error and diagnostics; `flockr job validate <id> --run-feed` shows what the command printed wrong. Usual cause: extra text on stdout |
| Handoff never asks | `flockr job run <id> --dry-run` shows the handoff verdict. Check `when` (a repeat of identical output is not a change), and `flockr decision list --all` for an identical earlier decision |
| Decision will not start | `flockr decision list` shows `last_error` (missing cwd, missing prompt file) |
| `rust-script` jobs are slow the first time | `flockr job build <id>` compiles ahead of time |

## Worked example 1: workbox-sync

Sync workbox hourly, show today's items in the sidebar, and offer to start Claude when the sync changed something.

```sh
flockr job new workbox-sync --template workbox
flockr job validate workbox-sync --run-feed
flockr job run workbox-sync --dry-run
flockr job enable workbox-sync
```

`~/.config/flockr/jobs/workbox-sync/job.toml`:

```toml
title = "workbox sync"
command = ["workbox", "sync"]
every = "1h"
timeout = "10m"
enabled = false
notify = "on_failure"

[feed]
command = ["sh", "-c", "workbox list --json | flockr feed adapt workbox --title 'Today (workbox)'"]
title = "Today (workbox)"

[handoff]
when = "on_change"
ask = "{{count}} items need you. Start Claude?"
agent_command = "claude"
cwd = "~/Desktop/poc/workbox"
tab_title = "workbox · {{date}}"
prompt_file = "prompt.md"
```

`prompt.md` is a template, for example: `Today is {{date}}. {{count}} items are unread: {{feed_json}}. Triage them; do not change anything until I say so.` The feed shows as `Today (workbox) N` in the sidebar, unread items first; `d`, `s`, `t` and `D` (delete, asks y/n first) run `workbox mark`. While trying this out, point `command` and the feed at a stub script instead of the real `workbox` so nothing in your tracker changes.

## Worked example 2: GitHub PR count

Every 30 minutes, print how many open pull requests the user has authored, and toast only when the number changes. It needs the `gh` CLI, authenticated in the login-shell environment (a `GH_TOKEN` exported from `~/.zshrc`, or `gh auth login`).

```sh
flockr job new pr-count --template shell --every 30m
```

`~/.config/flockr/jobs/pr-count/run.sh`:

```sh
#!/bin/sh
set -eu
count=$(gh search prs --author=@me --state=open --json number --jq length)
echo "open PRs: $count"
```

`~/.config/flockr/jobs/pr-count/job.toml` (the keys that differ from the generated file):

```toml
title = "open PR count"
command = ["sh", "run.sh"]
every = "30m"
timeout = "1m"
enabled = false
notify = "on_change"          # toast when the status or the stdout text changes
```

```sh
flockr job validate pr-count        # checks that gh resolves on the job's PATH
flockr job run pr-count --dry-run   # runs gh once and prints "open PRs: N"
flockr job enable pr-count
flockr job logs pr-count            # later: the last output
```

To also get a list in the sidebar, run `flockr job new` again with `--feed`, or add a `[feed]` whose command prints the version 1 object built from `gh search prs --author=@me --state=open --json number,title,url,repository` (for example with `jq`), and validate it with `--run-feed`.
