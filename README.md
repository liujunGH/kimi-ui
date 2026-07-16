# kimi-ui

[Kimi Code](https://www.kimi.com/code/) 官方 Web UI 的轻量桌面壳：Tauri 2 + 系统 WebView（macOS WKWebView），把 `kimi web` 变成一个独立桌面应用——不打包 Chromium，壳本身只占约 21MB 内存。

> A lightweight desktop shell for the Kimi Code web UI. Built with Tauri 2 and the system WebView (no bundled Chromium). The shell itself costs ~21MB of memory.

## 特性

- **独立应用形态**：独立窗口、Dock 图标、Cmd-Tab 直达、关窗即整体退出
- **原生窗口装饰**（macOS）：hidden-inset 标题栏，红绿灯悬浮，配合官方 Web UI 的桌面布局；侧栏头部/聊天头部可直接拖动窗口
- **原生通知**：补上了 WKWebView 缺失的 `Notification` API——任务完成/提问/审批三类提醒走 macOS 原生通知，点击回流激活窗口
- **下载支持**：会话导出等下载自动保存到 `~/Downloads`，重名自动追加 `(n)`
- **外链处理**：外部链接一律交给系统浏览器
- **零守护负担**：daemon 无人连接 60 秒自动退出，壳不管生命周期；下次启动自动拉起或复用
- **低内存**：壳主进程 ~21MB；整套（含 WebKit 服务进程与 SPA 本体）约 630MB，同 SPA 的 Electron 方案通常 900MB–1.2GB

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
4. 通过注入脚本补齐桌面能力（`Notification` polyfill、`window.focus()`、拖拽区镜像），除此之外不碰任何协议

## 维护说明

窗口拖拽区和"内部测试"角标的隐藏依赖官方 Web UI 的 DOM 选择器（`.side.macos-desktop .ch`、`.chat-header.macos-desktop`、`.internal-build-tag`）。**官方 Web UI 改版可能导致拖拽/角标失效**（不影响核心使用），修复点在 `src/main.rs` 的 `INIT_SCRIPT`。

## 目录结构

```
src/main.rs            # 全部壳逻辑（启动、注入脚本、下载、外链、命令）
public/index.html      # 启动占位页
capabilities/          # Tauri 窗口权限（拖拽）
scripts/icon.swift     # 图标生成器
packaging/             # Info.plist、make-app.sh、LaunchAgent plist
icons/                 # 生成的图标
```

## License

[MIT](LICENSE)
