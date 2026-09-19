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
- 已集成 Command Code 终端 agent：新增 `CLIAgent::CommandCode`（aliases `cmdc`/`command-code`/`commandcode`，slug `command-code`），resume/fork 按 `command-code --resume {id}` 及 `--fork-session` 生成，`--no-session` 视为免持久化；hooks 走 Claude 式 JSON map 写入 `~/.commandcode/settings.json`，注册 SessionStart/PreToolUse/PostToolUse/Stop 四个官方支持事件；图标沿用 bot.svg 兜底，accent 取文档站品牌黄 0xFAD000。
- 已复查 Command Code 集成：hook 写入形状与官方 schema 逐字段一致（外层 hooks 数组 + {type, command}，SessionStart/Stop 省略 matcher）；全部枚举引用点数据驱动无漏改；修复图标对比度——品牌黄圆盘配黑色 glyph（icon_rgb 0x000000），避免白色 glyph 在黄色背景上看不清。
- 已修复 Command Code 侧边栏双层会话行：无终端会话标题时，TabLabel::Agent 不再被当成第二行标题重复渲染；Agent 名称保留在第一行，第二行回退到工作目录，避免 Agent 名称与会话标题混在一起。
- 已修复 Command Code 状态在首轮结束后始终停留“已完成”：其无 UserPromptSubmit hook，工具型回合使用 PreToolUse 映射 prompt-submit，在首次执行工具前切回“进行中”，PostToolUse 记录活动，Stop 结束回合。
- 已修复 Command Code 进程识别与纯文本回合：移除会误识别 Windows `cmd.exe` 的通用 `cmd` alias，补充 npm 包实际提供的 `commandcode` alias；每个 Command Code Stop hook 计为一个完成回合，UI 同时比较 status 与 turns，使连续纯文本回合即使保持 Done 状态也能产生完成计数、未读标记和通知，其他 Agent 仍保留重复 Stop 去重。
- 已确认 Command Code 英文会话名称来自其自身 `.meta.json` 自动标题，tty7 原样显示以保持与 `/resume` 和 `/rename` 一致；中文标题可在 Command Code 内执行 `/rename 中文标题`。
- Command Code 侧边栏图标已从 bot.svg 兜底改为专属图标 command-code.svg：200×200 源图归一化为 24×24 透明背景描边图形（去掉纯黑背景矩形，避免 mask 渲染整面填充），在 `Assets::agent_icon` 注册嵌入，`icon_path()` 离开 fallback 组，fallback 断言列表同步移除 command-code；像素级效果仍待启动桌面窗口确认。

## 进行中

- 已实现品牌区、任务分区、仓库分组、两层 Agent 会话树和底部 workspace 入口，等待启动应用进行视觉检查。
- 已保留搜索、折叠、拖拽排序、关闭、右键菜单、Git Diff 和缩放行为。

## 待办

- 启动桌面应用进行侧边栏视觉检查。

## 阻塞

- 无。

## 最近验证

- `cargo fmt --check`：通过。
- `cargo check -p tty7`：Command Code 集成后通过，存在既有编译警告。
- `cargo test -p tty7-core --lib cli_agent`：34 个测试通过（含新增 Command Code 检测/resume/免持久化测试）。
- `cargo test -p tty7-core --lib agent_hooks`：35 个测试通过（含 Command Code hook 路径、安装回读写与 PreToolUse 回合开始映射测试）。
- `cargo test -p tty7 'ui::tray::icon::'`：5 个测试通过（含逐 agent 校验 glyph 颜色）；复查修正 icon_rgb 后回归通过。
- `cargo test -p tty7-core --lib core::tab_view`：8 个测试通过。
- `cargo test -p tty7 'ui::tab_sidebar::'`：31 个测试通过。
- `cargo check -p tty7`：侧边栏标题修复后通过，存在既有编译警告。
- `git diff --check`：通过。
- `cargo test -p tty7 'ui::tab_sidebar::'`：31 个测试通过。
- `cargo check -p tty7`：通过，存在既有编译警告。
- `cargo fmt --check`：Command Code 识别与纯文本回合修复后通过。
- `cargo test -p tty7-core --lib cli_agent`：35 个测试通过（新增 `cmd`/`cmd.exe` 排除、`commandcode` 检测及连续纯文本 Stop 计数回归）。
- `cargo test -p tty7-core --lib agent_hooks`：35 个测试通过。
- `cargo test -p tty7 'terminal::view::gpui_tests::a_'`：57 个测试通过（含连续 Done 回合、重建、重连及未读标记回归）。
- `cargo check -p tty7`：Command Code 三项修复后通过，存在既有编译警告。
- `cargo build`：使用 `rustc 1.98.1` 通过，存在既有编译警告。
- `cargo build --release --locked --target aarch64-apple-darwin`：通过，存在既有编译警告。
- macOS DMG：CRC 校验、只读挂载、`tty7.app` 与 `/Applications` 入口检查、ad-hoc 签名和三个可执行文件的 thin arm64 架构检查均通过；SHA-256 为 `c36edaf2c265a0a2622e648173e8ebe75f59769bd0562c6870af545d7ffe98c3`（2026-09-18 22:19 重新构建，包含品牌区 xview、分组标题 13px 与青色标题最新改动，27946031 bytes，同时生成 updater zip）。
- macOS DMG：CRC 校验、只读挂载、`tty7.app` 与 `/Applications` 入口检查、ad-hoc 签名和三个可执行文件的 thin arm64 架构检查均通过；SHA-256 为 `1e1bb457652260c156d9147a0e3cb0926748326b42ef5248dcfe36a7c4c01fc0`（2026-09-18 23:25 重新构建，包含 Command Code agent 集成与图标对比度修正，27947091 bytes，同时生成 updater zip，其 SHA-256 为 `dc971b9821c11003650512b3f965a7f4375007674eef89449ae91784f62c88f5`）。
- macOS DMG：侧边栏 Agent/会话行修复后重新构建并通过 CRC、只读挂载、`tty7.app` 与 `/Applications` 入口、ad-hoc 签名和三个可执行文件 thin arm64 架构检查；SHA-256 为 `cbb8143fb8f280fc7fcc4967dcfd063001fd2b3cb358615b010bb4b6ac94dffe`（2026-09-18 23:53，27947091 bytes）；updater zip SHA-256 为 `089d5e91af272a87e32e00554458d7605eb20a8343c282f32ef6d4491879fe56`。
- macOS DMG：Command Code PreToolUse 状态修复后重新构建并通过 CRC、只读挂载、`tty7.app` 与 `/Applications` 入口、ad-hoc 签名和三个可执行文件 thin arm64 架构检查；SHA-256 为 `f1c18607d55cebe0d9279b3004ff22fa9131e6d43ec7d750d20dc7efd47f7f21`（2026-09-19，27946891 bytes）；updater zip SHA-256 为 `483698b5df8a76bbe6a316fb35b971b643f9b30ef6ed2b5f44cf1b66f6d2da48`。
- macOS DMG：Command Code 专属图标落地后重新构建（`cargo build --release --locked --target aarch64-apple-darwin` + `--features updater --bin tty7-updater` + `bundle-macos.sh`）并通过 CRC 校验、只读挂载、`tty7.app` 与 `/Applications` 入口、ad-hoc 签名、三个可执行文件 thin arm64 架构检查，另确认 `icons/agents/command-code.svg` 已编入包内二进制；SHA-256 为 `ead260a9cad50a98a02a1cc411f22b6921fa706352798fae883e9535f071bbdf`（2026-09-19 09:19，27946665 bytes）；updater zip SHA-256 为 `38036955f2efc5e4e4ce213a002531b6e957b7c483006277a7af40d9c704b2ef`（23858675 bytes）。挂载点已卸载、临时目录已清理。
- macOS DMG：包含 Command Code 进程识别、`commandcode` alias 及纯文本回合状态修复的 Apple Silicon 版本已重新构建；release 与 updater 构建、DMG CRC、只读挂载、`tty7.app` 与 `/Applications` 入口、ad-hoc 签名、三个可执行文件 thin arm64 架构及 Command Code 图标资源检查均通过。DMG 为 `dist/tty7-26.9.2-macos-arm64.dmg`，SHA-256 为 `ec60d70242878617211ef58a0f76d026902d08c14c1f745d5a6e87a88109c23c`（2026-09-19 23:33，27946999 bytes）；updater zip SHA-256 为 `1dc30d7e30f82112668d0ff9707221b2c9bff2b0354aaf0b5f6ea0f33f7ee0ea`（23859453 bytes）。采用 ad-hoc 签名，适合本机测试，分发到其他 Mac 可能触发 Gatekeeper。
- `cargo fmt --check`：通过。
- `cargo test -p tty7-core --lib cli_agent`：34 个测试通过（含 Command Code 专属图标后的 fallback 断言）。
- `cargo test -p tty7 every_agent_icon_resolves`：通过（含 command-code.svg 注册校验）。
- `cargo check -p tty7`：通过，存在既有编译警告。
