# Mux

[![CI](https://github.com/Sunmedalia/mux/actions/workflows/ci.yml/badge.svg)](https://github.com/Sunmedalia/mux/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/Sunmedalia/mux)](https://github.com/Sunmedalia/mux/releases/latest)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Mux 是 Claude Code、Codex、Pi Agent 与 Grok 的多厂商、多模型配置管理器，支持保存和切换 Codex、Grok 账号。它维护 Endpoint、凭据、模型映射和本地协议代理；Herdr 用户还可以通过 Mux Pulse 侧栏查看 Token、账号额度和 Git 更改。同步完成后，在自己的终端运行对应客户端即可。

### 从旧名称升级

项目现已统一命名为 **Mux**：可执行命令为 `mux`，环境变量前缀为 `MUX_`，配置、状态与缓存目录名为 `mux`。旧命令与旧环境变量不再作为兼容入口。

首次启动时，如果 Mux 尚无配置，程序会识别旧名称的默认目录并转换 Provider、模型映射、已保存账号、客户端绑定及用量账本。旧目录保留为备份；发生修改的客户端配置会另存 `*.mux-migration-backup`。先关闭旧程序窗口，迁移完成后使用 `mux`。

旧安装使用自定义路径时，可以明确指定来源；目标路径由 `MUX_CONFIG` 和 XDG 目录变量控制。不会覆盖已有的 Mux 配置：

```bash
mux migrate --config /path/to/old/config.toml --state-dir /path/to/old/state --cache /path/to/old/models.json
```

迁移只转换本项目生成的标识与路径，账号凭据保持原样；中断的旧客户端事务需先用旧程序恢复。已安装的旧代理自启服务会转换为 Mux 服务；旧 Herdr 插件会保留文件并停用，由 Mux Pulse 接替。

它提供四个核心能力：

- 在 TUI 中管理厂商、模型目录、默认模型、角色别名与 1M 上下文。
- 将所有已启用模型聚合到 Claude 原生 `/model`，并实时同步启用状态。
- 把 Anthropic Messages 请求转发到 Anthropic、OpenAI Chat Completions 或 Responses 兼容网关。
- 在 Herdr 常驻侧栏中查看网关与会话用量，并完成 Git Diff、暂存、提交、分支、远端操作和提交历史搜索。

> v0.1.20 新增 Herdr Pulse Git 工作流与 TOKEN / GIT 互斥导航，详细变化见 [发布说明](docs/releases/v0.1.20.md)。Mux 支持 macOS ARM64、Linux x86_64/ARM64 和 Windows x64；Herdr 插件支持 macOS / Linux。Pi 可使用本地 Proxy API；四类客户端的 Provider 都支持单独配置模型目录地址。

[快速开始](#快速开始) · [快捷键](docs/navigation.md#tui-导航) · [Codex 配置与账号](docs/clients.md#codex-配置与账号) · [Pi Agent 配置](docs/clients.md#pi-agent-配置) · [Grok 配置](docs/clients.md#grok-配置) · [模型参数](docs/clients.md#模型-token-参数) · [同步](docs/clients.md#claude-model-同步) · [端口设置](docs/proxy.md#修改本地代理端口--多系统用户) · [Herdr Pulse](docs/pulse.md#herdr-pulse-常驻监控) · [卸载](docs/operations.md#卸载与配置清理) · [开发与测试](docs/development.md#开发)

## 安装

独立 Mux 程序的预编译包、校验和 Windows 说明见 [安装文档](docs/installation.md)。从源码运行需要 Rust 1.88+ / Cargo：

```sh
git clone https://github.com/Sunmedalia/mux.git
cd mux
cargo build --locked --release --bin mux
./target/release/mux
```

### Herdr 插件安装与更新

在 **Herdr 的普通终端**中，进入源码目录执行：

```sh
bash scripts/install-herdr.sh
```

需要支持插件的 Herdr 0.7.0+、Rust 1.88+ / Cargo 和系统 Git。脚本构建当前源码，更新 `~/.local/bin/mux`，把 `mux` 插件链接到当前目录并启用，配置 `prefix+u` 快捷键；已有的 Mux 快捷键会保留。默认按 **Ctrl+B，再按 u** 打开或关闭侧栏。插件直接从当前源码目录运行，请保留该目录。

更新时重新运行同一脚本，再退出并重开已有 Mux 窗口。检查安装结果：

```sh
herdr plugin list --plugin mux --json
herdr plugin action list
./target/release/mux --version
"$HOME/.local/bin/mux" --version
```

确认插件为 `enabled`、`plugin_root` 指向当前目录，操作列表包含 `mux` 的 `open`。若默认键位冲突，可用 `bash scripts/install-herdr.sh --key prefix+shift+u` 选择空闲键。源码目录被移动或删除时，应从新目录重新安装。使用 GitHub 插件安装或在线预编译安装的区别见 [Pulse 安装说明](docs/pulse.md#herdr-pulse-常驻监控)。

## 快速开始

```sh
mux doctor   # 检查 Claude、配置权限与网关连接
mux          # 打开 TUI
```

首次使用时：

1. 按 `a` 新建厂商，填写 API Endpoint、认证方式和凭据；需要单独的模型目录地址时填写 `Fetch models URL`，留空则从 Base URL 推导 `/models`。点击底部 `Fetch models` 或按 `Alt+F` 获取模型。之后可反复打开缓存列表，搜索后单击模型或按 `Enter` 回填当前字段，无需重复请求。`Ctrl+R` 或 `Refresh` 会重新获取；修改 Base URL、Fetch models URL、协议或凭据后也会重新获取。无需先保存厂商，也可手填模型 ID。
2. 进入厂商详情，按 `a` 打开添加模型；在添加模型表单中按 `Alt+F` 获取 API 模型目录，也可以手动填写模型 ID。厂商详情页不再提供整站模型刷新。
3. 用 `Space` 启用模型；按 `e` 编辑输出 Token 上限等参数，按 `1` 切换 1M 标记。禁用只暂停模型，不会删除模型。
4. 按 `p` 接入 Claude：自动启动本地代理，并将全部启用模型写入 Claude 设置。之后保存的变更会自动同步。
5. 在终端运行 `claude`，使用 `/model` 选择模型。退出 Mux 不会停止代理。

首次运行若检测到 `~/.claude/settings.json`，Mux 会显示脱敏导入预览。也可以手动执行：

```sh
mux import
mux import --yes
```


## Herdr Pulse：TOKEN 与 Git

侧栏顶部用 **TOKEN / GIT** 分段页签互斥选择，`●` 标记当前页，`○` 标记另一页；点击当前页不重置内容。**Alt+1 / Alt+2** 分别选择 TOKEN / GIT，也可按 `T` / `g` 直接选择；切回时保留 TOKEN 的统计子页、筛选和滚动位置，以及 Git 的文件选择、Diff 或 Log。处理操作或编辑表单时切换置灰，先完成或取消当前操作。TOKEN 页展示今日网关用量、当前会话 Token 和账号额度；Git 页跟随同一标签页最近聚焦终端的仓库，支持子目录和 worktree。`e` 打开完整 Mux 编辑器，`q` 关闭侧栏。

Git 使用固定仓库概览，展示项目、分支、上游和各类更改数量；点击分支名 **⑂ 分支 ▾** 打开分支选择页，选择后确认切换。文件按未暂存、已暂存和冲突分组，组标题带数量。选中行高亮，文件名与目录分层展示，新增 / 删除行数分别用绿 / 红色；长路径可换行并点击选择，短屏自动简化布局。

Git 文件分为 Unstaged、Staged 和 Conflicts；部分暂存文件可以同时出现在两个分组中。选择文件后使用：

| 操作 | 快捷键 / 按钮 |
| --- | --- |
| 查看当前文件 Diff | Enter / Diff |
| 暂存当前整个文件 | `s` / Stage file |
| 暂存仓库全部新增、修改和删除 | `a` / Stage all |
| 取消暂存当前文件或选中差异块 | `u` / Unstage |
| 确认后撤销未暂存文件或差异块 | `d` / Discard |
| 提交暂存内容 | `n` / Commit；Ctrl+Enter 预览确认 |
| 查看提交历史 | `L` / Log；Enter 查看提交详情与 Diff |
| 搜索当前分支全部提交的标题与正文 | Log 中 `/` / Search；Enter 搜索 |
| 清空搜索并恢复全部历史 | 搜索框内 Ctrl+U，再按 Enter |
| 分支、单文件/差异块暂存及 Fetch / Pull / Push | `b` / Branch 或 `o` / Ops |

Diff 默认自动换行，`w` 切换，关闭换行后用左右方向键横向滚动；`,` / `.` 切换文件，`[` / `]` 选择差异块，Esc 返回文件列表。Log 每页 50 条，`,` / `.` 翻页，搜索使用不区分大小写的字面文本匹配。状态和操作结果在底部按钮上方显示，按钮与提示随当前页面变化；返回用量首页只使用顶部 TOKEN 标签。详情页显示返回目标，提交确认页用 Edit 返回草稿；空表单、无匹配项和翻页边界的按钮置灰。Git 帮助按 `?`；完整操作范围、冲突和特殊文件说明见 [Git 侧栏文档](docs/pulse.md#git-侧栏)。

## 使用文档

- [安装与 Windows 说明](docs/installation.md)
- [TUI 导航、快捷键与表单](docs/navigation.md)
- [Claude、Codex、Pi、Grok 配置与账号](docs/clients.md)
- [本地代理、资源设置与用量统计](docs/proxy.md)
- [命令行、配置、安全、更新与卸载](docs/operations.md)
- [Herdr Pulse 常驻监控](docs/pulse.md)
- [Herdr Pulse Git 侧栏：Diff、暂存、提交、分支与远端操作](docs/pulse.md#git-侧栏)
- [开发、测试与性能基准](docs/development.md)

代理资源可以在 Settings → Proxy → Limits 中调整，默认允许 16 个在途请求、2 个 Token 估算任务和 32 MiB 正文；保存后重启代理生效。
