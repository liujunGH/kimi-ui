# 远程连接与多 daemon

桌面壳可以同时打开多个 daemon：本机自己拉起的那个（"本地"标签），以及**内网其他机器**上已经跑着的 daemon（"远程"标签）。点击标签即可切换。

## 远端机器要做的准备

远端 daemon 默认只绑 `127.0.0.1`，外部访问需要在**那台机器**上以非 loopback 方式启动：

```bash
kimi web --host 0.0.0.0 --insecure-no-tls --no-open
```

- `--host`（不带值或 `0.0.0.0`）绑定所有网卡。不带 `--host` 时只有本机可访问。
- `--insecure-no-tls` 不能省：官方对非 loopback 绑定会**拒绝启动**，除非明确接受无 TLS（`0.0.0.0` 被归类为 `public`）。
- 然后把该机器的 `~/.kimi-code/server.token` 内容复制过来，在壳的"＋ 连接"对话框里填地址、名称（可选）和令牌。

## 为什么不需要在远端配 CORS

壳的远程标签**直接加载远端 daemon 自带的官方界面**（`http://<远端>:<端口>/#token=...`），页面来源与 API 同源。官方 SPA 在没有 `kimi_origin` 参数时会以 `window.location.origin` 作为 API 地址（`kimi web` 打开浏览器用的就是这个流程）。同源请求天然通过官方 daemon 的来源校验，所以**不需要** `KIMI_CODE_CORS_ORIGINS`。

对比之下，"本地"标签走壳自己的回环静态服务（稳定 origin，界面偏好与哈希资源缓存都能复用）——这也是 0.1.13 起的做法，远程连接不适用：远端 bundle 版本可能与本壳不同，本地托管会串版本。

## 安全提醒

`--host 0.0.0.0` + `--insecure-no-tls` 意味着**同一局域网内任何拿到令牌的人都能完全控制那台机器上的 agent**（执行命令、读写文件）。请只在可信网络中使用，并且不要把地址或令牌外传。壳把这些信息存在 `~/Library/Application Support/dev.kimiui.desktop/connections.json`，权限 0600。

## 远程标签的能力边界

| 能力 | 本地 daemon | 远程 daemon |
| --- | --- | --- |
| 会话、聊天、文件 | ✅ | ✅ |
| 状态栏（上下文/额度） | ✅ | ✅（经壳侧代理） |
| 窗口标题、"会话"菜单 | ✅ | ✅ |
| 重启 daemon | ✅ | ❌（远端非 loopback 绑定默认禁用 `/shutdown`） |
| 内置终端 | ✅ | ❌（官方仅对 loopback 开放 PTY 路由） |

## 实现要点（维护者）

- 每个连接是一个独立的子 webview；`layout_strip` 统一排布，切换时只做 `show()`/`hide()`，页面保持存活。
- 远程连接的页面来源通过 `add_capability` 在运行时精确授权（仅该 daemon 的来源），不写宽通配。
- 状态栏不直接 `fetch` daemon：页面自身来源是 `tauri://localhost`，跨机时会被官方来源校验拒绝；统一走 `daemon_snapshot` 命令由 Rust 侧请求。
- 更新包的 tar 必须用 `tar --no-xattrs --no-mac-metadata`（见 `scripts/make-update-manifest.sh` 注释）。
