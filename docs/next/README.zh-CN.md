# flockr


<p align="center">
  <img src="assets/logo.png" alt="flockr" width="100" />
</p>

<p align="center">
  <a href="https://github.com/Rambarani/flockr">github.com/Rambarani/flockr</a> · <a href="#安装">安装</a> · <a href="https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/zh-cn/quick-start.mdx">快速开始</a> · <a href="https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs">文档</a></p>

<p align="center">
  <a href="README.md">English</a> · 简体中文
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-666666?labelColor=333333" alt="Apache 2.0 license" /></a>
  <a href="https://github.com/Rambarani/flockr/releases"><img src="https://img.shields.io/github/downloads/Rambarani/flockr/total?labelColor=333333&color=666666" alt="total GitHub release downloads" /></a>
  <a href="https://github.com/Rambarani/flockr/stargazers"><img src="https://img.shields.io/github/stars/Rambarani/flockr?labelColor=333333&color=666666&logo=github" alt="GitHub stars" /></a>
  <a href="https://github.com/Rambarani/flockr/releases/latest"><img src="https://img.shields.io/github/v/release/Rambarani/flockr?label=release&labelColor=333333&color=666666" alt="latest stable release" /></a>
</p>

---


**智能体复用器，住在你的终端里。**

- **每个智能体一目了然**——`blocked`、`working`、`done`。真实的终端视图，而不是包装过的转述。
- **分离后工作继续运行**——关闭客户端或 SSH 断线后，后台服务器仍会保持终端运行。服务器或机器重启后，Flockr 会恢复已保存的布局，并可恢复受支持的智能体会话；原有进程不会保留。[会话状态 →](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/zh-cn/session-state.mdx)
- **多台机器，一个窗口**——将本地工作和已保存的 SSH 机器放在一起，使用汇总的智能体列表，各连接独立重连。[远程机器 →](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/zh-cn/connecting-machines.mdx)
- **智能体也能使用 flockr**——纯 socket api：智能体可以创建窗格、读取输出、互相等待。[智能体技能 →](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/zh-cn/agent-skill.mdx) 在开发智能体？[为你的智能体添加 flockr 支持 →](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/zh-cn/add-flockr-support.mdx)
- **键盘和鼠标都是一等公民**——tmux 风格的前缀键，*以及*点击、拖动、分割。按当下的场景选择，而不是被工具锁死。
- **插件**——扩展窗格和工作流。[浏览插件市场 →](https://github.com/topics/flockr-plugin)
- **单个 rust 二进制，没有 electron**——运行在你已经在用的任何终端里。

---

## 安装

```bash
curl -fsSL https://raw.githubusercontent.com/Rambarani/flockr/main/distribution/install.sh | sh
```

或者 Windows：`powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/Rambarani/flockr/main/distribution/install.ps1 | iex"` · [受端点保护的 Windows](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/zh-cn/windows-beta.mdx) · [二进制文件](https://github.com/Rambarani/flockr/releases)

然后在工作所在的目录启动它：

```bash
flockr
```

运行你的智能体、分割窗格，然后安心离开。`ctrl+b q` 分离，`flockr` 重新连接。[快速开始 →](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/zh-cn/quick-start.mdx)

## 文档

所有文档都在 [github.com/Rambarani/flockr (docs)](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs)：[快速开始](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/zh-cn/quick-start.mdx) · [核心概念](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/zh-cn/concepts.mdx) · [受支持的智能体](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/zh-cn/agents.mdx) · [键盘](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/zh-cn/keyboard.mdx) · [配置](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/zh-cn/configuration.mdx) · [会话状态](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/zh-cn/session-state.mdx) · [连接机器](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/zh-cn/connecting-machines.mdx) · [远程访问](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/zh-cn/persistence-remote.mdx) · [集成](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/zh-cn/integrations.mdx) · [为智能体添加 flockr 支持](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/zh-cn/add-flockr-support.mdx) · [插件](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/zh-cn/plugins.mdx) · [socket api](https://github.com/Rambarani/flockr/blob/main/docs/next/website/src/content/docs/zh-cn/socket-api.mdx)

## 致谢

<a href="https://terminaltrove.com/"><img src="assets/sponsors/terminal-trove.png" alt="Terminal Trove" width="200" /></a>

[Terminal Trove](https://terminaltrove.com/) 以及 [SPONSORS.md](./SPONSORS.md) 中列出的每一位支持者——谢谢 🐑


## 智能体须知

如果你是协助本仓库的 AI 智能体：在改动代码前阅读 [`AGENTS.md`](./AGENTS.md)，在创建 issue 或 PR 前阅读 [`CONTRIBUTING.md`](./CONTRIBUTING.md)。

## 开发

```bash
git clone https://github.com/Rambarani/flockr
cd flockr
cargo build --release

just test        # 单元测试
just check       # 格式检查、测试和维护性检查
```

## 来源

flockr 是 [herdr](https://github.com/herdrdev/herdr)（Apache-2.0）v0.9.5 的衍生项目：重命名了产品，并新增侧边栏工作区分组和可配置的通知音效。详见 [NOTICE](./NOTICE)。

## 许可证

flockr 基于 [Apache License 2.0](LICENSE) 许可证发布。
