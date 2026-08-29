# 更新日志

本文件是版本说明的唯一来源：每个版本一节，标题格式固定为 `## 版本号 - 日期`。
打 `v*` tag 发版时，CI 自动截取对应段落作为 GitHub Release 的 notes；
应用内"更新提示"卡片展示的也是这里的内容。版本说明一律使用中文。

## 0.1.18 - 2026-08-29

- 新增应用内自动更新（macOS）：更新卡新增"自动更新"，后台下载官方 Release 的 zip 并按 sha256 摘要校验，一键原地替换当前应用并自动重启，无需再手动下载解压；替换失败自动回滚，Windows 继续引导浏览器下载
- 更新检查不再强依赖 gh CLI：优先用 gh（避开 API 限速），未安装时回退匿名 curl（仓库已公开）
- 更新卡保留"前往下载"链接作为回退；下载进度实时显示在卡片内

## 0.1.17 - 2026-08-29

- 官方 Web bundle 从 `@moonshot-ai/kimi-code@0.39.0` 升级至 `@moonshot-ai/kimi-code@0.39.1`（补丁版：工具行高度、IME 首字符、权限模式按会话隔离等修复），最低 CLI 版本同步升至 0.39.1；入口 JS 3,145,130 bytes，仍在本仓 3 MiB 预算内
- 新增原生菜单栏：应用/文件/编辑/会话/窗口/帮助六组中文菜单；⌘N 新建会话、⌘W 关闭窗口，编辑菜单补齐 WKWebView 的标准快捷键声明
- 窗口标题跟随当前会话（"<会话名> — Kimi Code"，Cmd-Tab 与调度中心可见）；"会话"菜单动态列出最近 8 个会话，点击直达（SPA 路由导航，无整页刷新）。macOS Dock 菜单 Tauri 2 尚无 API，属已知限制
- 新增 Remote Control 包装（官方实验特性）：状态栏"远程"按钮拉起 `kimi rc`，展示访问链接与二维码、支持复制链接与一键停止；壳退出时自动回收子进程。远程访问需 Kimi 账号登录，链接可控制本机，卡片内含醒目警告
- 移除蜂群名册卡与状态栏独立 WebSocket（官方 0.39 右侧多标签面板的 subagent 详情更完整），用量卡不再重复展示套餐额度详情（官方账户面板已有），状态栏保留"5h/周"条栏摘要
- 修复更新提示卡自 0.1.11 起被 28px 条栏裁剪的问题：usage/update/remote 任一卡片打开时浮层统一展开

## 0.1.16 - 2026-08-27

- 官方 Web bundle 从 `@moonshot-ai/kimi-code@0.38.0` 升级至 `@moonshot-ai/kimi-code@0.39.0`：新增 Remote Control（实验）、Tower 多 agent 编排（实验）、subagent fork（实验），web 右侧栏改为多标签面板，并修复同会话多客户端消息不同步、恢复中断会话反复崩溃、反复切换会话内存持续增长等问题
- 最低 Kimi Code CLI 版本提升至 0.39.0，旧版本启动时引导执行 `kimi upgrade`
- 升级前已核对全部壳耦合点：daemon 的 `/api/v1` REST 与 WS v1 在 0.39.0 仍默认挂载，`/api/v1/oauth/usage` 接口与 `kimi_origin` 桌面交接均未变化；入口 JS 2,808,393 → 3,101,530 bytes，仍在 3 MiB 预算内
- 启动 attach 时跳过版本低于已安装 CLI 的旧 daemon，改为拉起新的 `kimi web`：CLI 升级后重启 App 即生效，不再静默连用旧 server；被跳过的旧 daemon 经官方 `/api/v1/shutdown` 优雅关停（best-effort），不再长期占用端口与内存

## 0.1.15 - 2026-08-24

- 官方 Web bundle 从 `@moonshot-ai/kimi-code@0.33.0` 升级至 `@moonshot-ai/kimi-code@0.38.0`，跟随官方 8 个 release 的界面与协议更新
- 最低 Kimi Code CLI 版本提升至 0.38.0，旧版本启动时引导执行 `kimi upgrade`
- 入口 JS 性能预算从 2.5 MiB 调整为 3 MiB：官方 0.33.0 → 0.38.0 入口 JS 增长 34%（2,092,189 → 2,808,393 bytes），属官方功能增长，其余指标均在原预算内
- 套餐额度改用官方 daemon 接口 `GET /api/v1/oauth/usage`，移除无头 TUI PTY 抓屏方案与 `portable-pty`、`vt100` 依赖；重置时间按剩余时长在状态栏本地格式化
- 升级前已核对全部壳耦合点：daemon 发现（server/instances 注册表与 server.token）、`kimi web --port/--no-open` 参数、`kimi_origin` 桌面交接、状态栏 v1 REST/WS 接口均未变化；官方新增的 `/api/v2` 与 v1 并存，已记录迁移风险

## 0.1.14 - 2026-08-06

- 优化官方 Web 长会话渲染：用户 turn 与助手消息启用 `content-visibility`，真实 WKWebView 合成长历史基准的视口外布局耗时降低 25.8%
- 重构桌面注入脚本热路径：静态修复集中为单个样式表，DOM 观察器只扫描新增子树，不再在流式输出时反复全页查询
- 为内容哈希资源启用一年 immutable 缓存，入口与 SPA 路由继续 `no-cache`；缺失资源直接返回 404，避免缓存错误的 HTML 回退
- 新增官方 Web bundle 性能预算门禁，自动检查文件数、总体积、入口 JS/CSS 与最大资源，并在原子同步前拦截异常增长
- 新增性能基线、真实体验检查和官方上游优化清单；修正文档中仅统计壳主进程的低内存表述，明确完整 WebKit 内存口径与状态栏权限边界

## 0.1.13 - 2026-08-05

- Web UI 改为直接使用 Kimi Code 0.33.0 release 中的官方预构建 bundle，不再维护或构建 `apps/kimi-web` fork
- 构建与 CI 移除 Node/pnpm 前端编译步骤，固定同步官方 `apps/kimi-code/dist-web`，发布产物可复现
- daemon 地址交接改用官方 bundle 原生支持的 `kimi_origin`，继续保留 URL hash 凭据交接与桌面模式
- 修复已有 Kimi 服务占用默认端口时首次启动误连旧实例：壳显式选择空闲端口，并只等待自己拉起的服务
- 壳内 Web UI 改用独立稳定端口 51821，避免与官方 daemon 的 58627+ 端口冲突，并让语言、主题和 onboarding 状态可跨重启保留
- 最低 Kimi Code CLI 版本提升至 0.33.0，旧版本启动时会引导执行 `kimi upgrade`
- 修正 macOS App 的版本元数据，确保 Finder 与系统信息显示 0.1.13

## 0.1.12 - 2026-07-19

- 修复 release 白屏：嵌入资源收集改为递归（`include_dir` 的 `Dir::files()` 不含子目录，嵌套 JS/CSS 全部 404）；新增"index.html 引用资源必须可执行"回归测试拦截同类问题
- 修复打开/关闭调试（Web Inspector）后窗口底部被状态条遮挡：原生检查器会撑改 webview 框架且不缩回，切换调试后强制重排布局
- 修复远程源 ACL 静默拒绝自定义命令：通知 polyfill、`window.focus` 恢复可用，控制台 ACL 报错消除；`build.rs` 声明 app 命令清单，capabilities 按本地/远程来源拆分
- 构建加固：web-dist 原子换装（防半成品被嵌入）、`build/` 暂存 + 原子安装（防半截包与 bundle id 撞车）、安装时自动结束旧实例

## 0.1.11 - 2026-07-19

- web 资源编译期内嵌进可执行文件：macOS / Windows 均为单文件分发，不再附带 web 目录（开发时仍读磁盘 web-dist，改 web 不用重编译壳）
- 启动流程重构：已有服务实例直接 attach；旧版 CLI 走 `kimi server run`，新版 CLI（`kimi server` 已弃用）自动改为后台拉起 `kimi web --no-open`
- 壳拉起的服务进程在 App 退出时自动回收，不留孤儿进程
- 服务启动轮询上限 15 秒；子进程提前退出立即报错，并附带其 stderr 摘要
- kimi 版本检测：低于 0.26 拒绝启动；未安装、版本过旧、服务不可达分别给出带操作指引的错误页
- 状态栏更新提示现在显示完整版本说明（点击"⬆ 新版本"徽章查看）

## 0.1.10 - 2026-07-19

- 修复 Windows 发行包内 `web/` 目录无法被定位的问题：定制界面（滚动优化、工具组折叠、状态栏等）首次在 Windows 上生效

## 0.1.9 - 2026-07-19

- daemon 发现兼容适配：优先扫 `server/instances` 注册表（多实例取运行最久者），回退旧版 `server/lock`，候选全部 TCP 探活、跳过失效项——为上游多实例改造提前铺路
- 修复 Windows 启动出现黑色控制台窗口：release 改为 GUI 子系统，拉起子进程加 `CREATE_NO_WINDOW`
- 打包安装改为整体替换，修复陈旧 web 资源跨版本堆积（App 曾膨胀至 79MB）
- 新增 5 个 daemon 发现单元测试

## 0.1.8 - 2026-07-19

- 新增 Windows CI 构建与发行（实验性）
- 补齐 Windows 资源图标（icon.ico），修复 Windows 构建失败

## 0.1.7 - 2026-07-19

- 长会话性能：CSS 窗口化（content-visibility 阶段一）、工具组折叠即卸载、长输出尾部截断、滚动跟随重吸阈值收紧
- 移除会话历史保留上限，记录完整保留且不再卡顿
- README 按"daemon + 定制 web UI"架构重写
