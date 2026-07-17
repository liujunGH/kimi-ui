# Changelog

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
