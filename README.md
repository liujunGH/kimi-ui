# kimi-ui

[Kimi Code](https://www.kimi.com/code/) 的桌面端：**官方 daemon + 定制 web UI + 原生壳**。壳（本仓库）负责窗口、状态栏与系统集成；界面来自 fork 分支 [liujunGH/kimi-code `kimi-ui`](https://github.com/liujunGH/kimi-code/tree/kimi-ui) 构建的定制 kimi-web——滚动跟随、长会话性能、列表样式等体验问题都在源码级修复，不再依赖注入补丁。

> A desktop client for Kimi Code: the official daemon, a customized web UI (from our fork), and a native Tauri shell. [English README](README_EN.md)

## 特性

- **独立应用形态**：独立窗口、Dock 图标、Cmd-Tab、关窗即整体退出；hidden-inset 标题栏，拖拽区/双击缩放齐全
- **定制 web UI**（fork 源码级优化）：滚动跟随只在主动回到底部时恢复；长会话工具组默认折叠、折叠即卸载、大输出尾部截断；流式只重渲变化的 turn；离屏 turn 经 `content-visibility` 跳过布局绘制（窗口化阶段 1）；历史上翻页无上限
- **原生状态栏**：上下文用量、套餐额度（5h/每周）、忙闲灯、蜂群名册（子代理实时状态）、跟随/静止开关、更新提示徽标
- **原生通知 + Dock 角标**：完成/提问/审批走 macOS 通知，未读计数显示在 Dock
- **下载与外链**：会话导出自动存 `~/Downloads` 去重；外部链接走系统浏览器
- **界面自愈与三层看门狗**：官方改版导致 DOM/协议/格式漂移时明确告警，不静默坏掉
- **低内存**：壳主进程 ~26MB；系统 WebView，不打包 Chromium
- **CI 发版 + 更新提示**：Releases 直接下载 .app，应用内检测新版本

## 安装

**推荐：下载即用**——[Releases](https://github.com/liujunGH/kimi-ui/releases) 页面下载 `kimi-ui-macos-arm64.zip`，解压后拖入 `/Applications`。

前提只有一个：已安装并登录 [Kimi Code CLI](https://www.kimi.com/code/docs/en/)（`kimi` 命令可用，daemon、凭据都由它提供）。

## 从源码构建

需要 **两个仓库**（壳 + 定制 UI 源）：

```bash
git clone https://github.com/liujunGH/kimi-ui.git
git clone -b kimi-ui https://github.com/liujunGH/kimi-code.git

cd kimi-ui
bash scripts/build-web.sh     # 构建定制 web 包到 web-dist/（pnpm + corepack）
bash packaging/make-app.sh    # 编译壳 + 组包 + ad-hoc 签名
cp -R "Kimi Code.app" /Applications/
```

要求：Rust 工具链、Node ≥ 24.15（corepack 提供 pnpm）。

## 工作原理

1. 执行 `kimi server run`（幂等：daemon 未起则拉起，已起则复用）
2. 从 kimi 本地数据目录发现 daemon 地址（`server/instances` 注册表优先，回退旧版 `server/lock`，TCP 探活跳过失效项）并读取访问凭据
3. 内置静态服务（127.0.0.1:58628）托管定制 web 包，经 URL hash 完成凭据与 daemon 地址交接（与官方流程同构）；web-dist 缺失时回退 daemon 内嵌官方 UI
4. 状态栏是壳自有页面，直连 daemon REST/WebSocket；注入脚本只补桌面能力（通知、拖拽等）

## 与上游的关系

- fork 分支 `kimi-ui` 只改 `apps/kimi-web`，定期 rebase 上游 main
- 通用修复会回提上游：[#1898](https://github.com/MoonshotAI/kimi-code/pull/1898)（列表序号）、[#1899](https://github.com/MoonshotAI/kimi-code/pull/1899)（滚动跟随）、[#1900](https://github.com/MoonshotAI/kimi-code/pull/1900)（长会话性能）
- 壳专用改动（daemon 地址交接、相对资源路径）留在 fork，不提上游

## 维护说明

DOM/协议耦合点的失效都有三层看门狗告警（拖拽布局、状态栏 REST/WS、额度抓屏格式），修复点在 `src/main.rs` 的 `INIT_SCRIPT` 与 fork 的对应组件；官方改版最坏情况是功能退回原生行为，不会静默出错。

## 目录结构

```
src/main.rs            # 壳逻辑（窗口布局、静态服务、注入脚本、命令、额度采集、更新检查）
src/static_server.rs   # 零依赖静态服务（托管 web-dist）
public/index.html      # 启动占位页
public/status.html     # 原生状态栏
scripts/build-web.sh   # 从 fork 构建定制 web 包
scripts/icon.swift     # 图标生成器
packaging/             # Info.plist、make-app.sh、LaunchAgent plist
.github/workflows/     # CI 发版
capabilities/          # Tauri 窗口权限（拖拽 + 远程源 IPC）
icons/                 # 生成的图标
```

## License

[MIT](LICENSE)
