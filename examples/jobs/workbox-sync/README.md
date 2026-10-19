# workbox-sync: an hourly sync with a feed

This job runs `workbox sync` every hour and shows what `workbox list` returns
in a **Today (workbox)** section under the agent list in the Flockr sidebar,
and in the Feed overlay (`prefix+f`).

## Install

```sh
flockr job new workbox-sync --template workbox   # writes job.toml and prompt.md, disabled
flockr job validate workbox-sync                 # checks the file, the command and the templates
flockr job run workbox-sync --dry-run            # runs it once without recording anything
flockr job enable workbox-sync                   # start the schedule
flockr job run workbox-sync --wait               # sync now; the feed refreshes when it succeeds
flockr feed show workbox-sync                    # the items, their actions and ids
```

`job new` refuses to touch an existing job directory. To install by hand
instead, copy `job.toml` and `prompt.md` from this directory to
`~/.config/flockr/jobs/workbox-sync/` and set `enabled = true`.

## What you get

- **Sidebar**: a collapsible `Today (workbox) 3` section (3 is the unread
  count). Click a row to open the item, right-click for **done**, **seen** and
  **today**, click the header to collapse, click `+N more` for the overlay.
- **Feed overlay** (`prefix+f`): the full list. `enter` opens the link, `d`, `s`
  and `t` run the actions, `/` filters, `R` refreshes, `esc` closes. `x`
  marks items (`X` clears), and `w` opens a tab and starts Claude with the marked
  items (the current item when none is marked) as the prompt; see the
  commented `[feed.work]` block in `job.toml` for the working directory
  (`~/code/work/<repo>` for GitHub PRs) and prompt.
- **CLI**: `flockr feed list`, `flockr feed show workbox-sync`,
  `flockr feed refresh workbox-sync`, and
  `flockr feed action workbox-sync <item id> d`.

- **Handoff**: when a sync changes something, a toast asks `3 items need you.
  Start Claude?` and the question waits in the Jobs overlay's **Decisions** tab
  (`prefix+t`, then `tab`). `enter` or `s` opens a tab in `~/Desktop/poc/workbox`
  titled `workbox · <date>` running `claude` with `prompt.md` filled in; `z`
  snoozes for an hour, `x` dismisses, `p` previews the prompt. Flockr never
  starts the agent by itself.

## How it works

1. `workbox sync` pulls from your trackers into workbox's local store.
2. When it succeeds, Flockr runs the `[feed]` command with the job's
   environment (your login shell by default, so credentials from `~/.zshrc`
   reach workbox): `workbox list --json | flockr feed adapt workbox`.
3. The adapter turns each workbox item into a feed item: the id is workbox's
   item id, the badge its key, the state `unread` or `read`, the subtitle
   `<source> · <reason> · <where>`.
4. An action such as **done** runs `workbox mark <id> done` with the same
   environment, then the feed command runs again so the list drops the item.

5. After a successful run and its feed refresh, the `[handoff]` table decides
   whether to ask. `when = "on_change"` asks when the sync's stdout or the feed
   differs from the previous run. The question is deduplicated: the same items
   do not ask twice. `prompt.md` is a template: `{{count}}`, `{{date}}`,
   `{{feed_json}}`, `{{stdout}}`, `{{job.title}}` and the other names are listed
   in the Jobs docs. The rendered prompt is written to
   `<state dir>/jobs/workbox-sync/handoffs/<run>.md`.

If the feed command fails (for example you are offline), Flockr keeps the last
list and marks it stale: a `!` after the sidebar title, and the reason in the
overlay.

## Variants

- Today tab only: use the commented `command` in `job.toml`
  (`workbox list --tab today --json`).
- Different title: `flockr feed adapt workbox --title "..."`. A `title` in the
  `[feed]` table wins over it.
- No questions, just the list: delete the `[handoff]` table.
- Ask on every sync: `when = "always"`. Ask only about failures:
  `when = "on_failure"`.
- A different agent: `agent_command = "codex"`, or with the prompt file
  as an argument, `agent_command = "my-agent --prompt {prompt_file}"`.
- Hide the section but keep the feed: `[ui.sidebar.feeds] show = []` in
  `config.toml`, or pick feeds by job id with `show = ["workbox-sync"]`.

## Trying it without workbox

To see the feature without touching your workbox data, point the job at a stub
that prints a fixture array and a stub `mark` that only records its arguments;
the commands in `job.toml` need nothing but `workbox` on the job's `PATH`.
The array format is the one `workbox list --json` prints (`id`, `source`,
`key`, `title`, `where`, `reason`, `status`, `tab`, `actor`, `url`,
`updated_at`, `snippet`, `unread`, `today`, `snoozed_until`).
