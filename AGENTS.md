# kimi-ui 项目规范

Kimi Code 官方 web UI 的桌面壳：Tauri 2 + 系统 WebView（macOS WKWebView），单可执行文件分发，不是官方产品的 fork。

## 双仓架构

- 本仓 `liujunGH/kimi-ui`：壳（Rust/Tauri）。
- 兄弟仓 `liujunGH/kimi-code`（分支 `kimi-ui`）：定制 kimi-web 的源码，只改 `apps/kimi-web`；`scripts/build-web.sh` 用它构建出 `web-dist/`（需要 `KIMI_CODE_FORK` 指向 fork 本地路径，Node ≥ 24.15 + corepack pnpm）。
- fork 上的通用修复会回提上游 MoonshotAI/kimi-code；壳专用改动（daemon 地址交接、相对资源路径）只留 fork，不提上游。

## 构建与测试

- 提交前必须：`cargo build` 与 `cargo build --release` 均 0 警告，`cargo test` 与 `cargo test --release` 全过（release 下 web 资源走内嵌路径，两套覆盖不同代码路径）。
- 本地安装：`bash packaging/make-app.sh --install`（整体替换 /Applications，再手动重启验证）。
- UI 类改动必须截图亲眼验证后才算完成；截图只截必要小区域、验证后即删，**绝不**把含会话内容的截图放进仓库。

## 版本与发版

- 版本号保持 `0.1.x`，不动大版本；bump 时 `Cargo.toml` 与 `tauri.conf.json` 同步改，`cargo check` 刷新 `Cargo.lock`。
- 发版流程：提交 → 推 main → 打 `v*` tag → CI 双平台构建并上传 Release。
- **版本说明一律中文，唯一来源是 `CHANGELOG.md`**（标题格式 `## 版本号 - 日期`）。CI 发版时自动截取对应段落写入 Release，不要在 GitHub 上手写，也不要依赖自动生成的英文提交列表。
- 提交信息：英文、conventional 风格（`feat:` / `fix:` / `ci:` / `docs:`），发版提交形如 `0.1.x: 摘要`。
- git 提交/推送逐次经用户确认，不擅自操作。

## 代码规范

- `src/static_server.rs` 保持零依赖（std::net only）。
- release 编译期内嵌 `web-dist/`（`include_dir`），debug 读磁盘；`build.rs` 已挂 `rerun-if-changed`。
- Windows 兼容：release 走 GUI 子系统（无控制台黑窗）；拉起任何子进程必须经 `no_console()`（`CREATE_NO_WINDOW`）。
- 启动/连接错误用结构化类型（`BootError`），由占位页按类型渲染操作指引。
- 前端一律 `textContent` 渲染外部内容（Release notes、daemon 数据），禁止 innerHTML 注入。
- 代码注释用英文，用户可见文案用中文。

## 隐私与安全

- token、凭据、会话内容**绝不**进仓库、截图、版本说明。
- 守护进程地址/凭据只经本机回环与 URL hash 交接，与官方流程同构。

## 单一事实来源

- 启动与 daemon 发现流程：`src/main.rs` 顶部 crate 注释（改了流程必须同步注释）。
- 版本说明：`CHANGELOG.md`。
- 本文件有变更时，相关规范以最新提交为准。
