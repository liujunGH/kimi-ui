# kimi-ui

[Kimi Code](https://www.kimi.com/code/) 官方 Web UI 的轻量桌面壳：Tauri 2 + 系统 WebView（macOS WKWebView），把 `kimi web` 变成一个独立桌面应用——不打包 Chromium，壳本身只占约 21MB 内存。

> A lightweight desktop shell for the Kimi Code web UI. Built with Tauri 2 and the system WebView (no bundled Chromium). The shell itself costs ~21MB of memory.
> [English README](README_EN.md)

## 特性

- **独立应用形态**：独立窗口、Dock 图标、Cmd-Tab 直达、关窗即整体退出
- **原生窗口装饰**（macOS）：hidden-inset 标题栏，红绿灯悬浮，配合官方 Web UI 的桌面布局；侧栏头部/聊天头部可直接拖动窗口（双击缩放/还原）
- **原生通知**：补上了 WKWebView 缺失的 `Notification` API——任务完成/提问/审批三类提醒走 macOS 原生通知，点击回流激活窗口
- **Dock 角标**：未读通知计数显示在 Dock 图标上，窗口聚焦自动清零
- **原生状态栏**：窗口底部一条壳自己的状态栏（不占 SPA 区域、零遮挡）——实时显示上下文用量与模型；数据由状态栏直连 daemon REST/WebSocket 获取，与官方页面 DOM 零耦合，也是壳的拓展位
- **额度统计**：状态栏右侧显示套餐额度（5 小时 / 每周）与重置倒计时；官方暂无公开端点，由壳定期在内嵌伪终端（PTY）里无头执行 TUI `/usage` 并解析渲染结果——零外部依赖，失败自动降级
- **蜂群面板**：点状态栏的"蜂群"向上展开子代理实时名册——状态灯、swarmIndex、描述、结果摘要、token 用量（daemon `subagent.*` 事件流）
- **思考流防爬屏**：流式思考块限高内滚，长推理不再把页面顶得一直往上爬
- **下载支持**：会话导出等下载自动保存到 `~/Downloads`，重名自动追加 `(n)`
- **外链处理**：外部链接一律交给系统浏览器
- **界面自愈与自检**：有序列表两位数序号裁切修复；"内部测试"角标隐藏；官方界面结构变化时看门狗会提醒壳需要更新
- **零守护负担**：daemon 无人连接 60 秒自动退出，壳不管生命周期；下次启动自动拉起或复用
- **低内存**：壳主进程 ~26MB；整套（含 WebKit 服务进程、SPA 本体与状态栏小页）约 650MB，同 SPA 的 Electron 方案通常 900MB–1.2GB

## 要求

- 已安装并登录 [Kimi Code CLI](https://www.kimi.com/code/docs/en/) ≥ 0.26（`kimi` 命令可用；web UI、token 均由它提供）
- macOS 13+（代码含 Windows/Linux 分支但未实测）
- 构建需要 Rust 工具链

## 使用

首次从源码构建运行：

```bash
cargo build --release
./target/release/kimi-ui
```

打包成 `.app` 并安装（推荐，无需 tauri-cli）：

```bash
bash packaging/make-app.sh                 # 编译 + 组包 + ad-hoc 签名
cp -R "Kimi Code.app" /Applications/
```

开机自启（可选）：

```bash
cp packaging/dev.kimiui.desktop.plist ~/Library/LaunchAgents/
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.kimiui.desktop.plist
```

卸载自启：

```bash
launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/dev.kimiui.desktop.plist
rm ~/Library/LaunchAgents/dev.kimiui.desktop.plist
```

## 工作原理

壳本身不实现任何 Kimi 功能，只做"启动器 + 浏览器"：

1. 执行 `kimi server run`（幂等：daemon 未起则拉起，已起则复用）
2. 从 kimi 的本地数据目录读取 daemon 的监听地址与访问凭据（位置与格式同官方客户端）
3. 导航窗口到该地址，并按官方 Web UI 的标准方式完成凭据交接（与 `kimi web --open` 一致）
4. 主 webview 通过注入脚本补齐桌面能力（`Notification` polyfill、`window.focus()`、拖拽区镜像等）；底部的原生状态栏是壳自己的页面，用 daemon 地址与凭据直连 REST/WebSocket，完全不碰官方页面 DOM

## 维护说明

本壳有多处与官方 Web UI 的 DOM/协议耦合点，官方改版可能导致对应功能退化（不会搞坏东西，只会退回原生行为）：

- 拖拽区镜像：`.side.macos-desktop .ch`、`.chat-header.macos-desktop`
- 角标隐藏：`.internal-build-tag`
- 思考限高：`.tc-wrap:not(.is-collapsed) pre.tc`
- 列表序号修复：`.md ol`
- 状态栏：`/api/v1/sessions/*` REST 端点与 `subagent.*` 事件字段名（协议层，比 DOM 稳定）

DOM 修复点都在 `src/main.rs` 的 `INIT_SCRIPT`，看门狗检测到结构变化时会主动提醒；状态栏代码在 `public/status.html`。

## 目录结构

```
src/main.rs            # 全部壳逻辑（窗口布局、注入脚本、下载、外链、命令）
public/index.html      # 启动占位页
public/status.html     # 原生状态栏（用量、模型、蜂群名册）
capabilities/          # Tauri 窗口权限（拖拽 + 远程源 IPC）
scripts/icon.swift     # 图标生成器
packaging/             # Info.plist、make-app.sh、LaunchAgent plist
icons/                 # 生成的图标
```

## License

[MIT](LICENSE)
