# Roadmap

## 当前阶段

侧边栏视觉与信息层级重构。

## 已完成

- 已创建项目级开发约定并明确 UI 修改与验证规则。
- 侧边栏改动已通过 `cargo fmt --check` 和 `git diff --check`。
- 侧边栏改动已通过 `cargo build` 完整编译验证。
- workspace 创建与切换入口已从侧边栏顶部移到底部固定区域，任务树保持独立滚动，并继续复用现有切换面板及其新建入口。
- Agent 会话行已将 Agent avatar、Agent 身份和会话标题拆为明确的两层布局，并在两层之间绘制 L 形连接线。
- 侧边栏品牌区标题文字已从 tty7 改为 xview，仅改品牌区渲染文案，未动测试 fixture 与注释。
- 侧边栏分组标题（当前任务、Repo、Scratch 共用）文字与文件夹图标从 12px 调大到 13px，向 Agent 名称的 14px 靠近一档。
- 侧边栏分组标题文字颜色从 muted 灰改为 Git Diff 新增色 added_ink（青色），文件夹图标与 hover 变色行为保持不变。

## 进行中

- 已实现品牌区、任务分区、仓库分组、两层 Agent 会话树和底部 workspace 入口，等待启动应用进行视觉检查。
- 已保留搜索、折叠、拖拽排序、关闭、右键菜单、Git Diff 和缩放行为。

## 待办

- 启动桌面应用进行侧边栏视觉检查。

## 阻塞

- 无。

## 最近验证

- `cargo fmt --check`：通过。
- `git diff --check`：通过。
- `cargo test -p tty7 'ui::tab_sidebar::'`：31 个测试通过。
- `cargo check -p tty7`：通过，存在既有编译警告。
- `cargo build`：使用 `rustc 1.98.1` 通过，存在既有编译警告。
- `cargo build --release --locked --target aarch64-apple-darwin`：通过，存在既有编译警告。
- macOS DMG：CRC 校验、只读挂载、`tty7.app` 与 `/Applications` 入口检查、ad-hoc 签名和三个可执行文件的 thin arm64 架构检查均通过；SHA-256 为 `c36edaf2c265a0a2622e648173e8ebe75f59769bd0562c6870af545d7ffe98c3`（2026-09-18 22:19 重新构建，包含品牌区 xview、分组标题 13px 与青色标题最新改动，27946031 bytes，同时生成 updater zip）。
