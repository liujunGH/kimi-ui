# 远程控制设计方案：手机端遥控 Kimi Code 开发

> 状态：调研稿（未实施）。目标：手机端切换项目、查看会话、发送指令驱动开发、处理审批。
> 调研时间：2026-07-17（蜂群并行调研：metabot / codeg / 生态广泛搜索，均有源码级核实）。

## 0. 结论先行

**不自研 IM 桥。** 两个现成方案已覆盖需求：

1. **IM 驱动开发 → [cc-connect](https://github.com/chenhg5/cc-connect)**（14k★，MIT，**Kimi/Moonshot 官方赞助**，2026-07 仍活跃发版）：原生一等支持 Kimi CLI，飞书走 WebSocket 长连接**免公网 IP**。`npm i -g cc-connect` + 一份 TOML + 建个飞书自建应用，半小时内可用。
2. **全功能查看/操作 → Tailscale 内网直连 + 官方 web UI**：Mac 与手机同处 tailnet，手机浏览器直接开 `http://<mac-tailnet-ip>:<port>`。官方 UI 里会话/审批/蜂群全有，**零开发、功能无损、流量不出虚拟内网**。10 分钟。

自研桥（飞书长连接 ↔ kimi server REST/WS）仅在 cc-connect 不满足时再做（见 §4）。

## 1. 需求 vs 现成方案对照

| 需求 | cc-connect | Tailscale + web UI | codeg channels | metabot |
|---|---|---|---|---|
| 切换项目 | `/dir` ✅ | 工作区一等公民 ✅ | `/folder` ✅ | ❌（一 bot 一目录） |
| 查看会话/输出 | 流式回复 ✅ | 完整官方 UI ✅ | 工具细节+摘要 ✅ | 流式卡片 ✅ |
| 发 prompt 驱动 | `/new` 等 ✅ | ✅ | `/task` ✅ | ✅ |
| 远程审批 | `/mode` + permit 关键字 ✅ | 官方审批 UI ✅ | `/approve` `/deny` ✅ | ❌（yolo 硬编码） |
| 免公网 IP | 飞书长连接 ✅ | tailnet 内网 ✅ | 飞书长连接 ✅ | 飞书长连接 ✅ |
| 接入 Kimi 方式 | 原生（官方赞助） | 就是官方 UI | ACP（钉 0.26.0） | SDK 包 CLI（二线） |
| 与现有 daemon 会话互通 | 各自独立进程 | **同一份会话** ✅ | 不互通（自 spawn ACP） | 不互通 |

## 2. 方案详情

### 2.1 cc-connect（IM 路线首选）

- 形态：单进程桥，配置 TOML；13 个 IM 平台（飞书/钉钉/TG/Slack/Discord/企微/QQ/微信等）。
- Kimi 支持：13 种 agent 一等公民 + 任意 ACP agent；多项目单进程。
- 命令：`/dir` 切项目、`/new` `/list` `/switch` 管会话、`/mode yolo|default` 切权限、`@Bot` 或 permit 关键字审批、cron 定时任务、图片/文件回传（飞书首批支持）。
- 安装：`npm i -g cc-connect` 或 `brew install cc-connect`；飞书侧建自建应用开长连接事件订阅。
- 适合："想在 IM 里异步驱动开发"的场景。

### 2.2 Tailscale + 官方 web UI（全功能零开发）

- 两端装 Tailscale（免费档够），手机浏览器开 `http://<mac-tailnet-ip>:<port>`，可加主屏当 PWA。
- daemon 需绑非回环地址（`kimi server run --host <tailnet-ip>` 或 `--host 0.0.0.0`）：token 鉴权保持开启（**不要** `--dangerous-bypass-auth`），tailnet 本身就是私有网络。
- 审批、蜂群、会话管理全部是官方原生体验——这是唯一"功能无损"的方案。
- 替代：cloudflared 命名隧道 + Cloudflare Access（不想装 Tailscale 时；Quick Tunnel 的 SSE 有坑且 URL 会变，不推荐）。

### 2.3 codeg channels（已验证的重型备选）

- 飞书长连接（protobuf pbbp2 帧）已实现且活跃维护（v0.20.4，2026-07-16）。
- `/folder` 切项目、`/agent` 选 Kimi、`/task` 发任务、`/approve` `/deny` 审批、蜂群委派可见。
- 缺点：整套工作台（SQLite/会话库/自 spawn ACP），与现有 daemon 会话不互通；飞书端无按钮（全文本命令）。
- 适合：本来就想要多 agent 聚合工作台的人。

### 2.4 被淘汰项

- **metabot**：Kimi 是二线引擎（yolo 硬编码、无审批、不能切项目、thinking 不渲染），硬需求不匹配。
- **Happy**：原生 App 体验类目最佳（E2E + 推送 + 手机审批），但仅支持 Claude Code/Codex，接 Kimi 要 fork 写适配层——中期改造方向，非现成。
- **QQ 机器人**：官方平台资质审核 + 个人号协议风险，不推荐。
- **OpenClaw 类通用网关**：非 coding 专用，过重。
- **VS Code Remote Tunnels**：手机上是"桌面 UI 缩小版"，体验重；可作为兜底手段知道即可。

## 3. 推荐落地顺序

1. **今天（10 分钟）**：Tailscale 方案跑通——解决"躺着看一眼会话/点个审批"。
2. **本周（半小时）**：cc-connect + 飞书自建应用——解决"IM 里发任务驱动开发"。
3. 用一两周，若发现硬缺口（见 §4），再评估自研。

## 4. 什么时候才自研桥

仅当同时满足：① 必须用 **kimi server daemon 里已有的会话**（cc-connect/codeg 都是各自 spawn 进程，会话不互通）；② 需要 cc-connect 没有的深度定制（如蜂群事件精细呈现、自定义审批策略）。
届时直接打 `kimi server` REST/WS（能力已全部实测验证），飞书卡片/长连接踩坑可参考 metabot（MIT）`src/feishu/` 与 codeg `src-tauri/src/chat_channel/backends/lark.rs`。
