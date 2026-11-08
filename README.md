# ukk

**Native, extensible Multi-Agent Workspace.**

> **设计阶段 / Architecture-first** · GPUI Desktop + SwiftUI iOS + herdr-core + Relay + Plugins  
> **当前阶段：Runtime Spike 01 + Desktop Spike 02 + Live Sync Spike 03 + Durable Workspace Spike 04。** 已提交本地只读 Rust Host / hc-client 与 GPUI 桌面原生窗口源码，含真实终端快照读取；尚未经过真实 Mac + herdr-core 联调，也未确认 macOS GPUI 编译通过。下面的完整架构与功能仍是目标设计。

ukk 是一个从零打造的原生 Coding Agent 工作空间：同时运行多个 Codex、Claude Code 或其他兼容 Agent，保留可持久恢复的工作会话，通过 iPhone 扫码连接电脑，并在必要时以**悬浮小窗**查看或操控电脑屏幕。

**重要：** ukk 不 Fork `herdr-app`，只参考其功能并通过公共接口集成 `herdr-core`。Remote Screen 是可选插件，不是远程桌面模式。

## Architecture

```mermaid
flowchart TB
  subgraph Native["Native Clients"]
    GP["Desktop · Rust / GPUI<br/>Spaces · Sessions · Agents · Panes"]
    IO["iOS · SwiftUI<br/>Agent Chat · Approvals · Floating Screen"]
  end
  subgraph Network["Remote transport"]
    REL["Self-hosted relay candidate<br/>alma-relay · WSS / signalling"]
    TURN["P2P / TURN<br/>encrypted screen media"]
  end
  subgraph Host["Device · ukk Rust Agent Host"]
    API["Authenticated API + Event Sync"]
    MODEL["Domain Model + SQLite"]
    ADAPT["Structured Agent Adapters"]
    HC["herdr-core Client"]
    PLUG["Plugin Host + Permission Broker"]
    SCREEN["Remote Screen Plugin"]
    HCR["herdr-core Runtime<br/>persistent terminals / agents"]
    API <--> MODEL
    MODEL <--> ADAPT
    MODEL <--> HC
    HC <--> HCR
    MODEL <--> PLUG
    PLUG <--> SCREEN
  end
  GP <-->|Local IPC| API
  IO <-->|Encrypted control| REL
  REL <-->|Outbound WSS| API
  SCREEN <-->|WebRTC media| TURN
  TURN <-->|on-demand video + input| IO
```

**核心关系：** `Host → Spaces → Sessions → AgentRuns → herdr-core TerminalRefs`。Tabs/Panes 仅控制视图；关闭窗口不会终止 Agent。控制协议与屏幕媒体链路分离。

## 当前开发范围

**正在推进：** macOS、Windows、Linux（同一套 GPUI Desktop）＋ iOS / iPadOS（SwiftUI）。核心能力包含 herdr-core 多 Agent、持久 Sessions、自由分栏、插件架构、扫码配对和按需悬浮 Remote Screen。

**暂缓：** Android、Web、TUI。保留未来可通过统一协议接入的扩展空间，当前不创建客户端工程、不分配实现里程碑；不影响 Desktop / iOS 的功能完整性。

## Desktop platforms / 桌面跨平台

**macOS、Windows、Linux 都是 ukk Desktop 的一等目标平台。** 同一套 Rust + GPUI 原生 UI，共用 Host 协议与 herdr-core Runtime。macOS 使用 Metal，Windows 使用 Win32/DirectWrite，Linux 使用 Wayland/X11。

已加入三端构建检查，但还不能宣称三端已运行通过：当前 Unix Socket 服务于 macOS/Linux；Windows Named Pipe 服务端与客户端源码已经加入，但还需 CI、真机和安全验收。详见 [跨平台支持矩阵](docs/PLATFORM_SUPPORT.md) 和 [Windows IPC Issue #4](https://github.com/suj1e/ukk/issues/4)。

## Product direction

- **Multi-agent:** 动态 Agent 和子 Agent、并行会话，不预设固定角色流水线。
- **Native Desktop:** GPUI 原生 UI；Tabs、自由分栏、对话、Diff、Files、Git、Terminal、Activity。
- **Mobile Companion:** SwiftUI、扫码配对、设备管理、实时事件、审批、APNs 通知。
- **Floating Remote Screen:** 会话内浮窗，点击可展开控制，收起继续对话；查看与操控需独立授权。
- **Plugin Platform:** Manifest、命令、事件、原生视图槽位、插件权限代理、后台任务。
- **Persistent runtime:** herdr-core 持有实际执行；ukk Host 管理产品级状态与跨端同步。
- **Platform scope:** 当前优先完成 macOS / Windows / Linux 的 GPUI Desktop，以及 SwiftUI iOS / iPadOS；Android / Web / TUI 放入未来候选清单，暂不立项开发。Headless Host 仍属于多设备能力基础。

## Runtime Spike 01 — source now available

First executable Rust prototype: `ukk-hc-client` talks to herdr-core through `hc connect` in plain JSON-RPC mode, and `ukk-host` exposes a **private local read-only Unix socket** for `host.ping` and `terminal.list`. A mock protocol test and GitHub Actions workflow are included; real Mac verification remains pending.

```bash
cargo test --workspace
cargo run -p ukk-host -- doctor
cargo run -p ukk-host -- list
# Start Host in a separate terminal:
cargo run -p ukk-host -- serve
# Query the local Host socket:
cargo run -p ukk-host -- request terminal.list
```

See [Runtime Spike 01 — runbook and acceptance](docs/RUNTIME_SPIKE.md) for `hc` installation, live events and exact limitations.

## GPUI Desktop Spike 02

A new GPUI native desktop window is now in `apps/desktop` (not a herdr-app fork). It talks to the local `ukk-host` over the bounded `ukk-host-client` Unix socket adapter, displays real terminal/Agent runtime fields, supports selection and manual refresh, and clearly labels Host offline state. No fabricated Agent chats, fixed roles or remote desktop mode.

The GPUI dependency is isolated from root Cargo workspace CI and pinned to a Zed revision. The separate `GPUI Desktop (macOS)` GitHub workflow checks native compilation, but its outcome has **not been verified**.

```bash
# First start hc and ukk-host, then in another shell on macOS:
cargo run --manifest-path apps/desktop/Cargo.toml
```

See [Desktop Spike 02](docs/DESKTOP_SPIKE.md) for exact setup, limitations and Mac verification tasks.

## Live Agent State — Spike 03

The local Agent Host now subscribes to herdr-core `events.subscribe`, reconciles missed or restarted streams, and exposes **read-only**, versioned `host.snapshot` / `host.wait` methods over its private Unix socket. GPUI watches these Host revisions in the background: Agent status and dynamic child Agent rows update without manually refreshing.

```bash
# Terminal 1: start hc, then ukk Host
cargo run -p ukk-host -- serve
# Terminal 2: inspect event-backed snapshot
cargo run -p ukk-host -- request host.snapshot
# Terminal 3: launch Desktop on macOS
cargo run --manifest-path apps/desktop/Cargo.toml
```

**Status:** source, mock protocol and end-to-end test code committed; compilation/CI and real Mac testing are not yet confirmed. This is a local read-only event broker, not durable Session sync or mobile remote access.

See [Live Runtime Sync](docs/LIVE_EVENTS.md) for state ownership, API envelope, reconnection and acceptance checklist.

## Windows Named Pipe (source)

Windows 的本地 Host 接入已增加同用户 SID 限权 Named Pipe：`ukk-host serve`、`host.snapshot`、`host.wait` 使用与 macOS/Linux 相同的只读 API。请注意：**实现源码已提交，不代表 Windows 编译/安全验收已经通过**。详见 [Windows Named Pipe 设计与运行说明](docs/WINDOWS_IPC.md)。

## Durable Workspace / Session / Pane — Spike 04

已提交 `ukk-domain` 和 `ukk-store`：通过 SQLite 维护产品级 Space、Session、AgentRun 绑定和版本化 Pane 布局。Session 和 Pane 不再与 herdr-core 的终端生命周期混为一谈；关闭 Pane 或归档 Session **不会停止任何 Agent**。

GPUI 原生窗口现在可从 `workspace.snapshot` 读取 Space / Session 列表并展示已保存的分栏树（当前尚未支持拖拽调宽、原生聊天或 Diff 数据）。CLI 已支持创建 Space/Session、绑定真实终端、拆分/关闭 Pane；本地 RPC 保持只读，UI 元数据更改后目前需手动点击 Refresh。

```bash
cargo run -p ukk-host -- space-create ukk /your/project/path
cargo run -p ukk-host -- workspace-dump
# 用输出的真实 space_id 创建会话：
cargo run -p ukk-host -- session-create SPACE_ID "Implement sync protocol"
cargo run -p ukk-host -- workspace-dump
```

详见 [Workspace Persistence / SQLite 开发说明](docs/WORKSPACE_PERSISTENCE.md)。源码已提交，但三系统编译与实际 GPUI 运行尚未确认通过。

## Workspace 自动同步（Spike 05）

Host 已增加基于 SQLite v2 变更版本的 `workspace.state` / `workspace.wait` 只读接口。GPUI 会自动观察其他窗口或 CLI 对 Space、Session、Pane 布局的修改；herdr-core Agent 事件与 Workspace 元数据分别同步，互不拥有对方的生命周期。此功能在 `feat/workspace-live-sync` 开发分支中，尚待三端 CI 与真实窗口验收。

详细协议与验证步骤：[Workspace Live Sync](docs/WORKSPACE_LIVE_SYNC.md)。

## Documentation

| Document | Contents |
|---|---|
| [中文版总览](docs/OVERVIEW.zh-CN.md) | 产品方向、中文架构图、对象关系 |
| [Product Requirements](docs/PRODUCT.md) | 完整功能矩阵、用户场景和体验原则 |
| [Architecture](docs/ARCHITECTURE.md) | 端到端架构、组件边界和部署 |
| [Workspace Live Sync](docs/WORKSPACE_LIVE_SYNC.md) | SQLite 跨进程版本和 GPUI 元数据自动同步 |\n| [Workspace Persistence](docs/WORKSPACE_PERSISTENCE.md) | SQLite Space/Session/AgentRun、布局版本与开发运行命令 |
| [Domain Model](docs/DOMAIN_MODEL.md) | Spaces / Sessions / AgentRuns / Panes |
| [Plugin System](docs/PLUGINS.md) | 插件生命周期、能力代理、UI 槽位 |
| [Pairing](docs/PAIRING.md) | QR 扫码、设备授权与密钥 |
| [Remote Protocol](docs/PROTOCOL.md) | Commands、Events、重连与同步 |
| [Remote Screen](docs/REMOTE_SCREEN.md) | 悬浮屏幕、授权、WebRTC 传输 |
| [Security](docs/SECURITY.md) | 威胁模型、权限、风险与测试 |
| [Project Status](docs/status.md) | 已实现、CI、未完成验收与任务入口 |
| [Roadmap](docs/roadmap.md) | 完整产品开发阶段、依赖和并行 Agent 工作队列 |
| [Development](docs/DEVELOPMENT.md) | 预期代码目录和工程验证步骤 |
| [Runtime Spike 01](docs/RUNTIME_SPIKE.md) | Rust Host 原型、命令、模拟测试和真实 Mac 验收项 |
| [Windows IPC](docs/WINDOWS_IPC.md) | Windows Named Pipe、用户 ACL、安全约束、验收清单 |
| [Platform Support](docs/PLATFORM_SUPPORT.md) | macOS / Windows / Linux 一等桌面支持、OS 特定适配及测试清单 |
| [Desktop Spike 02](docs/DESKTOP_SPIKE.md) | GPUI 原生窗口、真实终端状态、macOS 构建验收项 |
| [Desktop Terminal](docs/GPUI_TERMINAL.md) | 只读终端观察、直接 hc-client 边界、安全后果与验收 |
| [Live Runtime Sync](docs/LIVE_EVENTS.md) | Host 实时订阅、事件缺口恢复、Desktop 自动刷新和测试 |
| [Upstream References](docs/REFERENCES.md) | 官方依赖、许可和待确认事项 |
| [ADR-0001](docs/adr/0001-platform-and-runtime.md) | GPUI + SwiftUI + herdr-core 的技术决策 |
| [ADR-0002](docs/adr/0002-remote-control-is-plugin.md) | 远控作为会话插件的交互决策 |
| [AGENTS.md](AGENTS.md) | Coding Agent 参与开发时必须遵循的约束 |

## First end-to-end goal

在 macOS / Windows / Linux 电脑上启动多个 Agent → 退出 Desktop 窗口但 Agent 继续运行 → iPhone 扫码连接并恢复会话 → 查看 Diff / 发送命令 / 审批 → 临时打开悬浮 Remote Screen 完成一项桌面操作 → 收起小窗继续 Agent 对话。

详细验收拆解见 [Roadmap](docs/roadmap.md)。

## Project status

文档基线已建立，已有本地 Host / hc-client、跨平台 GPUI Desktop 和 SQLite Workspace 原型源码（尚未完成三平台 CI 与实机验证）。GPUI 版本、herdr-core API 兼容、alma-relay 配对接入、iOS 真机、WebRTC 和 macOS 权限等都需要实际验证。

**Repository license:** 尚未决定（仓库目前为 Private）；参考项目的 Apache-2.0/MIT 不等于自动给 ukk 授权。
