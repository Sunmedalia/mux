# 配置与维护

[返回 Mux 概览](../README.md)

## 命令行

```sh
# 同步全部已启用模型；local 的默认模型作为 Claude 初始默认值
mux apply --profile local

mux config path
mux doctor
mux proxy status
```

## 配置

macOS / Linux 默认配置路径为 `~/.config/mux/config.toml`，Windows 为 `%APPDATA%\mux\config.toml`。支持 `MUX_CONFIG` 或 `XDG_CONFIG_HOME` 覆盖；执行 `mux config path` 查看实际路径。

```toml
version = 2

[profiles.local]
name = "Local gateway"
enabled = true
base_url = "http://127.0.0.1:18080"
api_format = "anthropic"
default_model = "claude-sonnet-4-6"
subagent_model = "claude-haiku-4-5"
fallback_models = ["claude-haiku-4-5"]
enabled_models = ["claude-sonnet-4-6"]
disabled_models = ["claude-opus-4-7"]

[profiles.local.credential]
kind = "bearer"
value = "replace-me"

[profiles.local.aliases]
opus = "claude-opus-4-7"
sonnet = "claude-sonnet-4-6"
haiku = "claude-haiku-4-5"

[[profiles.local.models]]
id = "claude-sonnet-4-6"
label = "Sonnet 4.6"
description = "Daily coding"
# 可选；根据上游模型能力填写，删除这两行即不设置
max_output_tokens = 8192
context_window = 32768
```

`api_format` 支持 `anthropic`、`openai-chat` 和 `openai-responses`。Endpoint 可以填写服务根地址、带 `/v1` 的地址或完整生成端点，Mux 会规范化路径并保留查询参数。

认证类型：

- `bearer`：`Authorization: Bearer`。
- `x-api-key`：`x-api-key` 请求头。
- `api-key`：字面量 `api-key` 请求头，适用于 Azure 类端点。
- `none`：无认证的本地网关。

版本 1 配置会在内存中自动迁移；旧配置缺少 `enabled` 时默认启用。下次保存后写为版本 2。

## 数据与安全

以下为 macOS / Linux 默认路径；Windows 路径见安装说明，XDG 环境变量可覆盖默认目录。

- 配置：`~/.config/mux/config.toml`
- 模型缓存：`~/.cache/mux/models.json`
- 代理状态与日志：`~/.local/state/mux/`
- 同步接入记录：`~/.local/state/mux/sync-state.json`（包含路径、默认厂商、变更标记及上次写入的受管字段快照，含本地代理认证信息，不保存上游凭据）

配置包含明文上游 Token，Unix 下强制使用 `0600` 权限。Token 不会写入缓存或运行日志。配置和缓存使用原子替换并带文件锁。编辑会合并其他实例对独立字段的修改；同字段冲突会要求重新打开编辑器。文件正在被其他实例写入时，TUI 会提示重试，不会一直等待文件锁。

## 卸载与配置清理

```sh
mux uninstall --dry-run  # 只查看清理清单，不修改文件；不带参数也是预览
mux uninstall --yes      # 停止代理、清理配置，并自动解绑本地 Mux Herdr 插件
mux uninstall --yes --herdr  # 要求 Herdr 检查成功；检查失败则中止卸载
```

执行前先关闭其他 Mux 窗口及 Pulse pane。卸载逐项清理当前路径对应的配置、缓存、代理注册表、日志、PID、同步状态和锁文件；只移除空的应用目录，不递归删除目录，也不扫描其他用户。如已安装 Herdr，普通卸载会检测并解绑经确认的本地 Mux 插件链接，同时移除 `mux.open` 快捷键；GitHub 托管的插件保留，需使用 `herdr plugin uninstall mux` 卸载。`--herdr` 要求 Herdr 检查成功，检查失败则中止卸载。其他 Herdr 配置保持不变。程序文件和源码 checkout 保留，可在配置清理成功后手动删除安装位置的 `mux` / `mux.exe`。

Claude 的 `settings.json`、聊天记录及其他应用文件保留。只有当 Claude 的地址和 token 仍能确认属于本 Mux 配置时，才清理对应备份，并按同步快照逐字段移除仍与上次写入一致的受管设置；已经切换到其他服务的 Claude 设置及备份原样保留。历史同步记录中登记的设置路径也会检查。没有快照的旧连接仅清除可确认归属的地址和 Token，保留未验证的模型字段。

为防止误删，卸载拒绝 HOME 外的自定义路径、符号链接、Windows reparse point、Unix 硬链接、跨用户文件、共享状态、损坏的配置和无法确认归属的自启项。此时会报错并要求先处理这些路径，不会扩大删除范围。`--yes` 不会绕过这些检查。旧版代理若不支持认证停止接口，需要先用旧版 `mux proxy stop` 停止。自启管理器失败或运行中的代理无法停止时保留配置；中途磁盘 I/O 失败会明确报告未完成，可修复后重试。

## 在线更新

```sh
mux update --check             # 查看 GitHub 最新 Release
mux update                     # 下载对应平台的发布包、校验 SHA-256 后更新程序
mux update --source .          # 在 Herdr 终端中快进更新当前源码并重新安装插件
```

Release 更新仅在版本号更新时执行，下载包必须带 GitHub 发布资产的 SHA-256 摘要；校验失败不会替换程序。源码更新要求当前分支没有未提交的已跟踪文件，并使用 `git pull --ff-only`，不会合并或覆盖本地提交。使用源码链接的 Herdr 插件请选择 `--source`；更新后重新打开 Mux/Pulse pane，代理可在请求空闲时重启以加载新程序。Windows 若锁定正在运行的 EXE，已校验的新文件会留在原目录，关闭 Mux 后按命令输出的路径替换。
