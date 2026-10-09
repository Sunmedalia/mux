# Mux

[![CI](https://github.com/Sunmedalia/mux/actions/workflows/ci.yml/badge.svg)](https://github.com/Sunmedalia/mux/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/Sunmedalia/mux)](https://github.com/Sunmedalia/mux/releases/latest)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Mux 是 Claude Code、Codex、Pi Agent 与 Grok 的多厂商、多模型配置管理器，支持保存和切换 Codex 订阅账号。它负责维护 Endpoint、凭据、模型映射和本地协议代理，但不会启动 Claude；同步完成后，直接在自己的终端运行 `claude` 即可。

### 从旧名称升级

项目现已统一命名为 **Mux**：可执行命令为 `mux`，环境变量前缀为 `MUX_`，配置、状态与缓存目录名为 `mux`。旧命令与旧环境变量不再作为兼容入口。

首次启动时，如果 Mux 尚无配置，程序会识别旧名称的默认目录并转换 Provider、模型映射、已保存账号、客户端绑定及用量账本。旧目录保留为备份；发生修改的客户端配置会另存 `*.mux-migration-backup`。先关闭旧程序窗口，迁移完成后使用 `mux`。

旧安装使用自定义路径时，可以明确指定来源；目标路径由 `MUX_CONFIG` 和 XDG 目录变量控制。不会覆盖已有的 Mux 配置：

```bash
mux migrate --config /path/to/old/config.toml --state-dir /path/to/old/state --cache /path/to/old/models.json
```

迁移只转换本项目生成的标识与路径，账号凭据保持原样；中断的旧客户端事务需先用旧程序恢复。已安装的旧代理自启服务会转换为 Mux 服务；旧 Herdr 插件会保留文件并停用，由 Mux Pulse 接替。

它提供三个核心能力：

- 在 TUI 中管理厂商、模型目录、默认模型、角色别名与 1M 上下文。
- 将所有已启用模型聚合到 Claude 原生 `/model`，并实时同步启用状态。
- 把 Anthropic Messages 请求转发到 Anthropic、OpenAI Chat Completions 或 Responses 兼容网关。

> 本文对应 v0.1.19，首个 Mux 发布版。支持 macOS ARM64、Linux x86_64/ARM64 和 Windows x64。Pi 可使用本地 Proxy API；Claude、Codex、Pi 和 Grok 的 Provider 都支持单独配置模型目录地址。

[快速开始](#快速开始) · [快捷键](docs/navigation.md#tui-导航) · [Codex 配置与账号](docs/clients.md#codex-配置与账号) · [Pi Agent 配置](docs/clients.md#pi-agent-配置) · [Grok 配置](docs/clients.md#grok-配置) · [模型参数](docs/clients.md#模型-token-参数) · [同步](docs/clients.md#claude-model-同步) · [端口设置](docs/proxy.md#修改本地代理端口--多系统用户) · [Herdr Pulse](docs/pulse.md#herdr-pulse-常驻监控) · [卸载](docs/operations.md#卸载与配置清理) · [开发与测试](docs/development.md#开发)

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
