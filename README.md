# flockr


<p align="center">
  <img src="assets/logo.png" alt="flockr" width="100" />
</p>

<p align="center">
  <a href="https://github.com/Rambarani/flockr">github.com/Rambarani/flockr</a> · <a href="#install">install</a> · <a href="https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/quick-start.mdx">quick start</a> · <a href="https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs">docs</a>
</p>

<p align="center">
  English · <a href="README.zh-CN.md">简体中文</a>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-666666?labelColor=333333" alt="Apache 2.0 license" /></a>
  <a href="https://github.com/Rambarani/flockr/releases"><img src="https://img.shields.io/github/downloads/Rambarani/flockr/total?labelColor=333333&color=666666" alt="total GitHub release downloads" /></a>
  <a href="https://github.com/Rambarani/flockr/stargazers"><img src="https://img.shields.io/github/stars/Rambarani/flockr?labelColor=333333&color=666666&logo=github" alt="GitHub stars" /></a>
  <a href="https://github.com/Rambarani/flockr/releases/latest"><img src="https://img.shields.io/github/v/release/Rambarani/flockr?label=release&labelColor=333333&color=666666" alt="latest stable release" /></a>
  <a href="https://formulae.brew.sh/formula/flockr"><img src="https://img.shields.io/homebrew/v/flockr?label=homebrew&labelColor=333333&color=666666" alt="Homebrew version" /></a>
  <a href="https://x.com/Rambarani"><img src="https://img.shields.io/badge/follow-%40Rambarani-000000?logo=x&logoColor=white" alt="follow @Rambarani on X" /></a>
</p>

---

https://github.com/user-attachments/assets/043ec09f-4bdd-41d5-aee0-8fda6b83e267

**the runtime your coding agents live on.**

- **detach without stopping work** — flockr keeps terminals running in a background server when you close the client or lose your SSH connection. after a server or machine restart, flockr restores the saved layout and can resume supported agent sessions; the original processes do not survive. [session state →](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/session-state.mdx)
- **several machines, one window** — keep local work and saved ssh machines together, with a combined agent list and independent reconnects. [remote machines →](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/connecting-machines.mdx)
- **never hunt for the stuck one** — every pane is marked working, blocked, or idle. when an agent stops and needs an answer, flockr says so.
- **agent-native** — agents drive flockr through the cli and socket api: they can spawn panes, prompt each other, and wait until another agent is genuinely blocked. [agent skill →](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/agent-skill.mdx)
- **runs what you already run** — claude code, codex, cursor, opencode, grok and the rest. flockr doesn't wrap or replace them; it owns their terminals. building an agent? [add flockr support →](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/add-flockr-support.mdx)
- **keyboard and mouse, both first-class** — tmux-style prefix keys *and* click, drag, split. pick per moment, not per tool.
- **plugins** — extend panes and workflows. [browse the marketplace →](https://github.com/topics/flockr-plugin)
- **one rust binary, no electron** — runs in whatever terminal you already use.

---

## install

```bash
curl -fsSL https://raw.githubusercontent.com/Rambarani/flockr/main/distribution/install.sh | sh
```

or `brew install flockr` · `mise use -g flockr` · windows: `powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/Rambarani/flockr/main/distribution/install.ps1 | iex"` · [endpoint-protected Windows](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/windows-beta.mdx) · [binaries](https://github.com/Rambarani/flockr/releases)

then start it where the work lives:

```bash
flockr
```

run your agents, split panes, walk away. `ctrl+b q` detaches, `flockr` reattaches. [quick start →](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/quick-start.mdx)

## docs

everything lives at [github.com/Rambarani/flockr (docs)](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs): [quick start](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/quick-start.mdx) · [concepts](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/concepts.mdx) · [supported agents](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/agents.mdx) · [keyboard](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/keyboard.mdx) · [configuration](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/configuration.mdx) · [session state](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/session-state.mdx) · [connecting machines](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/connecting-machines.mdx) · [remote](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/persistence-remote.mdx) · [integrations](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/integrations.mdx) · [add flockr support to your agent](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/add-flockr-support.mdx) · [plugins](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/plugins.mdx) · [socket api](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/socket-api.mdx)

## thanks

every past sponsor and backer is listed in [SPONSORS.md](./SPONSORS.md) — thank you 🐑

enterprise / partnership: github.com/Rambarani/flockr/issues

## agent instructions

if you are an ai agent helping with this repository, read [`AGENTS.md`](./AGENTS.md) before making changes and read [`CONTRIBUTING.md`](./CONTRIBUTING.md) before opening issues or PRs.

## development

```bash
git clone https://github.com/Rambarani/flockr
cd flockr
cargo build --release

just test        # unit tests
just check       # formatting, tests, and maintenance checks
```

## license

Flockr is licensed under the [Apache License 2.0](LICENSE).
