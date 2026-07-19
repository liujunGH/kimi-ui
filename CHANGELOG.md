# Changelog

## 0.1.6 (2026-07-19)

- **CI 自动发版**：`.github/workflows/release.yml`——打 `v*` tag 即在 GitHub Actions 构建 web 包 + 打包 .app 并上传 Release（macOS arm64 zip），别人下载即用，无需拉两个仓库编译
- **应用内更新提示**：启动时经 gh CLI 检查最新 Release，状态栏出现"⬆ 新版本"徽标，点击打开下载页
- **窗口状态记忆**：`tauri-plugin-window-state`——窗口大小/位置/最大化重启还原
- **构建脚本可移植性**：`build-web.sh` 不再依赖本机 nvm 路径（CI/其他机器可直接跑）
- **fork web 历史上限**：已加载消息封顶 600 条，超出停止向上翻页（保留在线尾部），长会话内存增长封死

## 0.1.5 (2026-07-18)

- **壳托管定制 UI（架构升级）**：壳内置零依赖静态服务（`127.0.0.1:58628`），托管 fork 构建的 kimi-web；daemon 地址与 token 经 URL hash 交接（与官方流程同构），官方 daemon 原样使用、官方通道升级。web-dist 缺失时自动回退 daemon 内嵌 UI
- 新增 `scripts/build-web.sh`：一键从 fork（`~/project/kimi-code` 或 `KIMI_CODE_FORK`）构建并同步 UI 到 `web-dist/`
- **滚动静止开关**：状态栏"跟随/静止"切换——静止时通过自有 `scrollTop` 属性屏蔽 SPA 的程序化滚动赋值（原生滚轮/触摸不受影响），流式期间不再被拽回底部

## 0.1.4 (2026-07-17)

- **额度采集去 tmux 化**：改用内嵌伪终端（`portable-pty` + `vt100` 精确复现屏幕）无头执行 TUI `/usage`——零外部依赖，.app 开箱即用，不再要求 `brew install tmux`

## 0.1.3 (2026-07-17)

- **官方更新不适配检测（三层看门狗）**：
  - DOM 层：每 20s 探测拖拽/角标/思考限高选择器，失效时原生通知点名失效功能
  - 协议层：状态栏轮询区分"daemon 未连接"（灰显，临时态）与"接口结构变化，壳需要更新"（红字告警）
  - 抓屏层：额度连续解析失败时栏上显示"额度 –"（可见降级，不再静默消失）

## 0.1.2 (2026-07-17)

- **额度统计**：状态栏显示套餐额度（5h/每周百分比 + 重置倒计时），用量详情卡含完整两项
- 实现：无公开额度端点（REST/云端/RPC 全量排查无果），改为无头 TUI 方案——隐藏 tmux 会话执行 `/usage` 抓屏解析；沙箱 KIMI_CODE_HOME 运行，不产生会话垃圾，失败静默降级
- 蜂群卡片抗规模：已完成子代理超过 5 个自动折叠，点击展开

## 0.1.1 (2026-07-17)

- **原生状态栏**（壳自有 UI 层，取代浮窗方案）：上下文用量、模型、忙闲灯；点击浮出详情卡；数据直连 daemon REST/WebSocket，与官方页面 DOM 零耦合
- **蜂群面板迁入状态栏**：覆盖式浮窗名册（主页面不再移位）；快照播种 + 实时事件，重启不丢
- **双击标题栏缩放/还原修复**：根因是 capabilities 缺 `remote.urls`——远程源页面的所有 IPC 一直被静默拒绝（窗口拖拽、通知 polyfill 同因修复）
- 移除菜单栏托盘与界面内浮窗（HUD）
- 新增远程控制设计稿：`docs/remote-bot-design.md`
- 构建：多 webview 布局（`unstable` feature）、透明 webview（`macos-private-api`）

## 0.1.0 (2026-07-16)

- 首个版本：Tauri 2 + 系统 WKWebView 壳，daemon 启动器 + token 交接
- hidden-inset 标题栏、拖拽区镜像、"内部测试"角标隐藏
- 通知 polyfill、Dock 角标、下载处理、外链系统浏览器
- 思考流限高防爬屏、有序列表序号裁切修复、看门狗自检
