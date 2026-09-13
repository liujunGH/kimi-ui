# kimi-ui

[Kimi Code](https://www.kimi.com/code/) 的桌面端：**官方 daemon + 官方 Web UI + 原生壳**。壳（本仓库）只负责窗口、状态栏、系统集成与跨平台打包；界面直接使用 Kimi Code release 中提交的官方预构建 Web bundle，不再维护 Web UI fork。

> A desktop client for Kimi Code: the official daemon and Web UI in a native Tauri shell. [English README](README_EN.md)

## 特性

- **独立应用形态**：独立窗口、Dock 图标、Cmd-Tab、关窗即整体退出；hidden-inset 标题栏，拖拽区/双击缩放齐全
- **官方 Web UI**：直接跟随 Kimi Code release 的预构建 bundle，界面功能、协议与官方版本保持一致
- **多 daemon 标签**：本地与内网其他机器的 daemon 各占一个标签，点击切换；远程连接见[说明](docs/remote-connections.md)
- **原生菜单栏**：应用/文件/编辑/会话/窗口/帮助中文菜单，⌘N 新建会话；"会话"菜单动态列出最近会话，点击直达
- **会话感知窗口标题**：标题跟随当前会话，Cmd-Tab 与调度中心一眼可辨
- **壳自有状态栏**：本地受信 WebView 展示上下文用量、套餐额度（5h/每周）、忙闲灯、跟随/静止开关和更新提示徽标
- **Remote Control 包装**（官方实验特性）：一键拉起 `kimi rc`，展示访问链接与二维码，从手机或其他电脑继续本机会话
- **原生通知 + Dock 角标**：完成/提问/审批走 macOS 通知，未读计数显示在 Dock
- **下载与外链**：会话导出自动存 `~/Downloads` 去重；外部链接走系统浏览器
- **界面自愈与三层看门狗**：官方改版导致 DOM/协议/格式漂移时明确告警，不静默坏掉
- **系统 WebView**：不打包 Chromium；完整内存包含官方页面和 WebKit 辅助进程，随会话规模变化，基线与预算见 [性能文档](docs/performance.md)
- **CI 发版 + 应用内自动更新**：Releases 供下载；macOS 在应用内完成下载（minisign 签名校验）、原地替换与重启，Windows 引导浏览器下载
- **可重启的本地 Engine**：状态栏"重启"按官方流程优雅关停并原址拉起新 daemon（等 pid 变化才算成功）

## 安装

**推荐：下载即用**——[Releases](https://github.com/liujunGH/kimi-ui/releases) 页面下载 `kimi-ui-macos-arm64.zip`，解压后拖入 `/Applications`。

前提只有一个：已安装并登录 [Kimi Code CLI](https://www.kimi.com/code/docs/en/)（`kimi` 命令可用，daemon、凭据都由它提供）。

## 从源码构建

需要本仓和一个固定在官方 release tag 的 `kimi-code` checkout：

```bash
git clone https://github.com/liujunGH/kimi-ui.git
git clone --branch '@moonshot-ai/kimi-code@0.39.1' --depth 1 https://github.com/MoonshotAI/kimi-code.git

cd kimi-ui
KIMI_CODE_REPO=../kimi-code bash scripts/build-web.sh  # 同步官方 dist-web
bash packaging/make-app.sh --install                  # 编译、组包并安装
```

要求：Rust 工具链；无需 Node/pnpm。若 `kimi-code` 不在默认的 `~/project/kimi-code`，请设置 `KIMI_CODE_REPO`。

## 工作原理

1. 检查 kimi CLI 是否安装及版本（< 0.39.1 引导 `kimi upgrade`，未安装引导官方文档）
2. 发现已有服务实例则直接 attach（版本低于已安装 CLI 的旧 daemon 会被跳过并优雅关停，保证升级生效）；否则选择空闲端口并后台拉起 `kimi web --no-open`（App 退出时回收），再从 `server/instances` 注册表（回退旧版 `server/lock`，TCP 探活跳过失效项）发现地址、读取访问凭据
3. 内置静态服务（127.0.0.1:51821，避开官方 daemon 的 58627+ 端口段）托管官方 web 包（release 编译期内嵌进 exe，单文件分发；开发时从 web-dist 磁盘读取），通过官方支持的 `kimi_origin` 参数和 URL hash 交接 daemon 地址与凭据；稳定 origin 保留界面偏好，内容哈希资源使用长期缓存，包缺失时回退 daemon 内嵌官方 UI
4. 状态栏是壳自有页面，经壳侧代理读取 daemon REST（上下文用量、套餐额度、Remote Control 状态）——跨机连接时页面自身来源无法直连远端 daemon；注入脚本只补桌面能力（通知、拖拽、会话路由上报等）；更新提示带版本说明（Release notes 由 CI 自动生成），macOS 支持应用内自动更新（官方 updater 插件 + minisign 签名校验）

## 与上游的关系

- Web UI 直接取自官方 `@moonshot-ai/kimi-code@0.39.1` 的 `apps/kimi-code/dist-web`
- 旧 `liujunGH/kimi-code` 的 `kimi-ui` 分支仅作为历史备份，不再 rebase 或发版
- 界面与协议问题提交 Kimi Code 上游；本仓只维护桌面窗口、原生桥接、状态栏和打包

## 维护说明

DOM/协议耦合点的失效都有看门狗告警（拖拽布局、状态栏 REST/WS、套餐额度接口），壳侧修复点集中在 `src/main.rs` 的 `INIT_SCRIPT`；官方改版最坏情况是功能退回原生行为，不会静默出错。同步 bundle 会自动执行资源预算门禁，性能基线、真实体验清单和上游待办见 [docs/performance.md](docs/performance.md)。

## 目录结构

```
src/main.rs            # 壳逻辑（窗口布局、静态服务、注入脚本、命令、套餐额度、更新检查）
src/static_server.rs   # 零依赖静态服务（托管 web-dist）
public/index.html      # 启动占位页
public/status.html     # 壳自有的本地受信状态栏
scripts/build-web.sh   # 同步官方预构建 web bundle
scripts/check-web-performance.sh # 官方 bundle 资源预算门禁
scripts/icon.swift     # 图标生成器
packaging/             # Info.plist、make-app.sh、LaunchAgent plist
.github/workflows/     # CI 发版
capabilities/          # Tauri 窗口权限（拖拽 + 远程源 IPC）
icons/                 # 生成的图标
```

## License

[MIT](LICENSE)

版本说明见 [CHANGELOG.md](CHANGELOG.md)（发版自动引用），项目协作规范见 [AGENTS.md](AGENTS.md)。
