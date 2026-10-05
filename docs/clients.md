# 客户端配置

[返回 Mux 概览](../README.md)

## 客户端配置隔离

配置版本为 v6：Claude 厂商保存在 `[profiles]`，Codex 保存在 `[codex.profiles]`，Grok 保存在 `[grok.profiles]`。Pi TUI 直接读取 Pi 的 `models.json` 和 `settings.json`；旧的 `[pi.profiles]` 仅供兼容 CLI 导入/同步使用，不再作为 Pi 页面数据源。四个客户端的模型缓存分别保存。

旧版 v1–v3 的共享厂商会在迁移时复制为三份独立列表，以保留已添加的模型；之后编辑不再互相影响。Codex/Pi 副本不保留 Claude 角色别名。已有账号和接入快照保留，首次保存写入 v6；v4/v5 配置迁移后 Grok 列表为空，旧版本 Mux 不能编辑 v6 文件。

## Grok 配置

点击顶部 **Grok**，或按 `F2` 切换。页面沿用 Claude 的厂商和模型管理操作，支持三种 API 协议。Grok 的 **API Provider** 与 **OAuth Account** 可以共存：选择 OAuth 只改变默认模型，已启用的 API 模型仍可通过 Grok 的 `/model` 选择，并经 Mux 本地代理转发、记录 Gateway token。按 `p` 可将 API 模型设为默认模型。

1. 按 `i` 查看脱敏导入预览，按 `Enter` 导入已有自定义模型和常用设置；也可通过 `a` 新增厂商。
2. 编辑厂商、模型、启用状态及 token 参数。新增模型在 Grok 中使用 `mux::厂商ID::模型ID`，请求发送实际上游模型 ID；导入模型保留原有配置键。
3. 选择厂商或模型，按 `p` 接入所有厂商的全部启用模型并设置启动默认值（不是只同步当前选择的模型）。接入后，保存的修改自动同步。禁用当前默认模型时，改用其他启用模型；全部禁用时恢复可用的原生默认设置。
4. 重启 Grok 加载配置，再使用 `/model` 查看并切换全部已同步模型；也可运行 `grok models` 检查加载结果。不同厂商的同名模型使用独立配置键，分别保留端点和凭据。`s` 查看接入状态，`D` 断开并恢复管理字段。

配置目标为 `$GROK_HOME/config.toml`，默认 `~/.grok/config.toml`；Windows 默认 `%USERPROFILE%\.grok\config.toml`。API Key 在界面和导入预览中脱敏，保存文件使用私有权限。

按 `F4`，选择 **Grok** 打开 **Grok settings**：默认模型、搜索模型、分叉子代理模型、推理强度、权限模式、紧凑显示和思考内容显示。模型字段可手动输入原生模型 ID，`Alt+M` 循环选择已配置模型键；推理强度使用左右键选择，`Alt+E` 切换为自定义输入。空值或 `inherit` 保留原生设置，`Ctrl+S` 保存，`Esc` 取消；未保存变更会提示是否丢弃。

外部修改了 Mux 管理字段时，自动同步暂停。按 `p` 查看冲突字段，确认后重新接入。断开和卸载仅恢复未被外部修改的管理字段；无关配置、MCP、插件、Hooks 和登录凭据保留。不管理 Grok 多账号切换或任意高级 TOML 字段。仅包含继承端点的模型条目会在导入预览中标记并保留，需手动添加完整端点后管理。

Provider 列表包含独立的 **Grok OAuth Account** 行。全屏选中时，右侧直接显示 OAuth 账号状态；按 `Enter` 或 `o` 可在右侧管理登录、原生模型、Wake 与退出。窄屏则进入独立账号页，按 `Esc` 返回 Provider 列表：`b` 启动浏览器登录，`d` 使用设备码（适合远程终端），页面显示授权链接和设备码；`Esc` 取消正在进行的授权。凭据写入、刷新和退出登录由原生 Grok 处理，登录状态来自本地凭据；账号用量通过官方只读账单接口获取。可用 `MUX_GROK_BIN` 指定 Grok 可执行文件。

登录后，填写原生模型（默认 `grok-build`），按 `u` **Use OAuth** 将其设为启动默认模型，然后重启 Grok。Use OAuth 会保存 API Provider 的启用状态、暂停这些 Provider 并移除 Grok 代理路由；再次选中 API Provider 按 `p` 会恢复原来的启用状态和 API 默认模型。登录本身不切换模式；已有厂商、模型和登录凭据都会保留。显式模型 API 配置优先于 OAuth，因此 Use OAuth 要求没有本地覆盖的原生模型及原生模型目录端点。`r` 异步刷新本地登录状态和账号用量，`w` 对当前 OAuth 账号执行 Wake，`x` 确认退出登录；退出只清除原生登录凭据，保留 API 厂商配置。授权方式见 [Grok 官方认证说明](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-pager/docs/user-guide/02-authentication.md)。

进入账号页或登录成功后自动获取用量：显示当前周期的已用比例、剩余比例、额度进度条、重置时间，以及服务返回的预付余额和按需用量/上限。共享额度会标为 **Shared account credit allowance**。这些数据是账号额度，不是本地会话 token 统计；缺失字段不会显示为零。`PgUp/PgDn` 查看较长详情。刷新失败时保留本次运行中同一账号的上次结果并提示缓存状态；退出登录或更换账号会清除对应缓存。访问令牌过期时，先在 Grok 刷新登录或重新授权，再按 `r`。接口依据 [Grok 官方账单实现](https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-shell/src/extensions/billing.rs)。

实现依据 [Grok 原生设置说明](https://docs.x.ai/build/settings)，兼容验证基准为 Grok Build CLI `1.0.41`。

## Pi Agent 配置

> v0.1.7 包含 Pi 原生文件管理；兼容验证基准为 Pi `0.85.1`。

点击顶部 **Pi** 标签或按 `F2` 切换到 Pi 配置管理。首页与 Claude Code 使用相同的 `Mux Providers · F2 <下一个 Agent> · N providers` 标题和厂商布局。Pi 的厂商、模型、目录缓存和接入配置独立管理，编辑与导入不会更改 Claude/Codex。

进入 Pi 标签即读取 `PI_CODING_AGENT_DIR` 指定目录，默认 `~/.pi/agent`。无需导入：页面展示 `models.json` 中可编辑的自定义厂商，包括已有 `mux-*` 条目；由 Proxy API 生成的入口不重复显示。新增、编辑、删除保存时直接修改原条目，不写入 Mux 的 `[pi.profiles]`。状态栏显示实际读取目录。

Pi 默认直连厂商。选中厂商或模型后按 `P`（或点击 **Proxy API**）可开启 Mux 本地代理：Mux 为该厂商在 Pi 的 `models.json` 中添加 `mux-proxy-<厂商 ID>`，并把选中的模型设为 Pi 默认模型。原始厂商、上游地址和凭据保持可编辑；代理从原始厂商读取配置，代理请求会计入 Pi 的 Usage。再次按 `P` 可移除代理厂商并恢复直连默认值。代理运行地址显示在厂商详情中；使用代理时 Mux 代理进程需要保持运行。

| 按键 | 操作 |
| --- | --- |
| `i` | 重新读取 Pi 原生配置文件 |
| `a` | 导航焦点新增厂商；模型或详情焦点新增模型，保存到 `models.json` |
| `e` | 厂商页面编辑厂商；模型页面编辑模型 |
| `E` | 编辑所属厂商 |
| `x` | 删除选中的厂商或模型，确认后写回文件 |
| `p` / Set default | 将选中厂商和模型写为 `settings.json` 的默认值 |
| `P` / Proxy API | 开启或关闭选中厂商的本地代理入口 |
| `s` | 查看实际配置目录、可编辑厂商数量和只读条目原因 |

表单保存即生效，不需要先连接或同步。Pi 原生文件没有厂商/模型启用开关，配置中的模型都可用；列表统一显示 configured（已配置），不显示启用/禁用状态，模型表单也不再提供 Enable now 开关。单击仅选中条目，再次点击进入；`Space`、`A`、`C` 不修改文件，需要移除时使用 `x` 删除操作。删除当前默认厂商会清除默认引用，删除当前默认模型会改用该厂商的替代模型。`p`（或模型页的 `d`）仅设置默认选择，其他 `settings.json` 设置保留。

可用以下命令检查原生配置路径和读取结果：

```sh
mux pi files
```

以下旧版 CLI 命令仍保留，操作 Mux 中的 Pi 副本并生成 `mux-*` 条目，与原生 TUI 编辑不同：

```sh
mux pi import --dry-run       # 预览导入，不执行凭据命令
mux pi import
mux pi apply --profile deepseek
mux pi apply --profile deepseek --model deepseek-v4-flash
mux pi status
mux pi disconnect
```

Pi **直接连接厂商**，不启动或依赖 Mux 代理。三种格式对应 Pi 原生 `anthropic-messages`、`openai-completions`、`openai-responses`。API Key 会写入 Pi 配置，Unix 文件权限为 0600；字符串按 Pi 规则转义。输出上限由 Pi 客户端使用，不是代理强制限制。

- 目标目录遵循 `PI_CODING_AGENT_DIR`，默认 `~/.pi/agent`。TUI 保留原厂商 ID，`settings.json` 仅修改 `defaultProvider` / `defaultModel`。
- 模型 ID 移除 `[1m]` 后缀；显式 Context window 优先，否则 `[1m]` 对应 1,000,000。Max output tokens 对应 Pi 的 `maxTokens`。其他能力使用 Pi 默认值，导入模型保留其兼容性、输入类型和推理能力配置。
- 自定义 API 厂商和普通 API Key 可编辑；内置模型覆盖、OAuth、命令或环境变量凭据、混合协议及每模型独立认证等暂不支持的条目会保留在文件中，并在状态中列出只读原因，不执行凭据命令。
- 原有 Pi 厂商、主题、扩展、订阅登录、会话保留；不会修改 `auth.json`。原厂商与 `mux-` 项可同时出现在 Pi 中。本次不提供订阅多账号管理。
- 在 Pi 内重新打开 `/model` 可重新加载目录；启动默认值请在新 Pi 进程中确认。命令行、项目设置和扩展可能覆盖全局配置。
- 原生编辑保留厂商、模型中的额外字段（例如 `compat`、`cost`、`reasoning`、`input`）。写入使用文件锁、原子替换和事务日志；中断后重新加载恢复，遇到并发文件修改会提示重试。直接编辑无需断开管理。

验证真实 Pi 进程（隔离 HOME、本地模拟上游，不使用真实凭据）：

```sh
cargo build
SMOKE_FORMAT=anthropic python3 tests/fixtures/pi_cli_smoke.py
SMOKE_FORMAT=openai-chat python3 tests/fixtures/pi_cli_smoke.py
SMOKE_FORMAT=openai-responses python3 tests/fixtures/pi_cli_smoke.py
```

可使用 `MUX_TEST_BINARY` 和 `MUX_PI_BIN` 指定 Mux / Pi 二进制。

## Codex 配置与账号

> Codex 配置随 v0.1.7 发布。CLI 与 ChatGPT App 内的 Codex 使用同一套目标配置。切换账号时，Mux 会在磁盘写入后重启正在运行的 Codex 后台服务，让普通 `codex` 的新会话读取新账号；这会断开服务中的现有会话，因此 TUI 会先提示。若 Mux 本身运行在 Codex 任务中，切换只写入磁盘，并提示任务结束后运行 `codex app-server daemon restart`。真实 App 的账号切换与新会话请求仍需在目标版本上验证。

在 TUI 中点击顶部 **Claude / Codex / Pi / Grok** 标签，或按 `F2` 循环切换，按 `F3` 切换 Codex 的 API Providers / Accounts。四个标签分别读取独立的厂商和模型配置；修改只作用于当前客户端；Pi 直接管理原生配置文件，没有启用/禁用和代理同步操作。Codex 的 API 页面不显示 Claude 的角色别名设置。

### Codex API

1. 在 Codex 的 API Providers 页面添加或选择厂商。
2. 进入厂商页面选中模型；`e` 编辑模型、`E` 编辑厂商，`g` 设置推理强度。
3. 按 `p` / **Apply Codex**，同步 Codex 配置中所有已启用厂商的已启用模型，并将所选模型设为启动默认值。Mux 启动独立的聚合代理，并写入 `model`、`model_catalog_json` 和专属 `model_providers.mux`。
4. 首次接入后重启 Codex CLI。在同一会话中使用原生 `/model` 选择已登记模型，可跨厂商切换，无需再次重启；请求由代理转发到所选厂商。模型列表使用 `厂商ID::模型ID` 区分同名模型，并附带“厂商名 · 模型名”。
5. 新增或修改模型后，再按 `p` 并重启 Codex CLI 加载新目录。Codex 的目录仅在启动时加载；禁用或删除模型后，代理立即拒绝新请求，但旧进程的列表可能仍显示它。

Mux 显示的是磁盘启动默认值，不代表运行中会话正在使用的模型。按 `p` 不会改变已有会话的选择；请在 Codex 中通过 `/model` 切换。ChatGPT 订阅账号继续独立管理，不会加入 API 模型列表；桌面端仍需在目标版本验证。

支持 OpenAI Responses、Chat Completions 和 Anthropic 厂商。Responses 上游直接转发；另外两种格式转换文本、图片（取决于上游能力）、函数工具、命名空间工具、自定义编辑工具与流式输出。无法转换的内容返回明确错误，包括跨协议的加密推理历史、`previous_response_id` 和托管工具；转换型厂商默认关闭 Codex 托管网页搜索。远程 `/responses/compact` 只转发给 Responses 上游，其他上游需客户端本地压缩。

应用时为所有已启用的 Codex API 模型生成 `model_catalog_json`，避免 Codex 提示模型元数据缺失。每个模型保留各自的上下文容量；未配置时暂用 128K，`[1m]` 使用 1M。不写入固定的全局上下文或压缩阈值覆盖，让 Codex 按当前模型的目录元数据处理。默认只声明文本与基本工具能力，不假定第三方模型支持 Codex 托管工具或原生推理参数；目录中包含 Chat Completions 或 Anthropic 厂商时关闭托管网页搜索。切换订阅及断开管理时恢复原目录设置。

发往上游时，代理去除厂商命名空间和 Claude 专用 `[1m]` 后缀，使用真实模型 ID。`Max output tokens` 在代理侧限制实际输出；`Context window` 写入每个模型的目录元数据。推理强度仍需所选上游模型支持；转为 Chat Completions 时，目录默认的 `none` 不作为 `reasoning_effort` 发送，而是使用上游默认行为，并不保证关闭上游推理。上游返回结构化错误时，代理显示参数和具体原因，并过滤本地及上游认证凭据。跨协议切换保留已有兼容性检查：无法转换的历史会明确报错，不会静默丢弃。

```sh
mux codex apply --profile my-provider --model my-model --reasoning high
mux codex status
mux codex disconnect
```

可用本地模拟供应商验证真实 CLI 的 `/model` 切换（无 API 凭据，隔离配置，POSIX 环境）：

```sh
cargo build
python3 tests/fixtures/codex_model_switch.py
```

该检查验证同一进程、同一会话跨供应商切换，保留对话历史，并确认新增模型需同步和重启后出现。可用 `MUX_CODEX_BIN` 指定 Codex CLI。

### Codex 订阅账号

Codex 首页依次显示 **All Models**、**ChatGPT Account** 和 API 提供商。**All Models** 汇总所有已启用厂商的已启用模型；单击选中，再次点击或按 Enter 打开所属厂商，按 `p` 同步目录并设置选中模型为启动默认值。

选中首页的 **ChatGPT Account**，按 **Space** 启用或禁用订阅配置，弹窗确认后生效（Enter / y 确认，Esc / n 取消，也可点击按钮）。首次启用前先 Enter 进入账号页，导入账号并用 Space 选中；之后会记住已使用的账号。

- **启用订阅**：保存所有 API 厂商当前的启用状态，自动关闭它们并应用所选 ChatGPT 账号；TUI 在 ChatGPT Account 卡片标记订阅 Enabled、原先开启的厂商标记 Paused by ChatGPT。All Models 保持与 Claude 相同的 API 模型汇总视图，此时为空。
- **关闭订阅**：恢复被自动关闭的厂商及其模型；原本手动关闭的厂商继续关闭。优先恢复先前使用的 API 模型；该模型已不可用时选择一个恢复启用的模型；没有可用 API 模型时断开 Mux 管理并恢复原配置。
- 切换订阅账号不会覆盖保存的 API 启用状态。订阅期间 API 厂商保持关闭；先禁用订阅再启用 API 厂商。新增厂商不会被自动恢复为开启。
- 普通 API 厂商和模型的启用/禁用不弹窗；订阅的启用/禁用必须确认，包括通过 Apply 从 API 模式切入订阅。账号页的 `p` / Apply 直接应用当前高亮账号；Space 只作预选。

订阅与 API 模式之间切换后需重启 Codex。API 模式内已加载的模型仍可通过原生 `/model` 切换，无需重启。CLI 可用 `mux codex accounts disable` 关闭订阅并恢复 API 配置。

账号页提供导入、切换、浏览器登录、额度刷新与 Wake：

| 按键 / 按钮 | 操作 |
| --- | --- |
| `i` / Import | 导入本机当前 Codex 登录，输入保存名称 |
| `I` / File | 导入指定 `auth.json` 文件 |
| `↑↓`、`j/k` | 选择账号 |
| `Space` | 预选光标所在账号，不应用配置 |
| `p` / Apply Codex | 应用当前高亮账号，同时切换为 ChatGPT 提供商 |
| `r` / Refresh | 刷新选中账号的额度 |
| `w` / Wake | 唤醒选中账号并刷新额度 |
| `R` / Reset | 查询所选账号的重置卡，二次确认后兑换一张并刷新额度 |
| `Esc` / Back | 返回提供商列表 |
| `?` | 与 Claude 一致的分栏 Help：Providers / Accounts / Models / Forms |

Mux TUI 的 Codex / Grok 账号管理页中的 **Wake**（或 `w`）发送一次最小 prompt 后刷新额度，可能消耗少量用量；不自动重试，也不批量唤醒其他账号。Codex 使用隔离的临时登录副本和不持久化会话；Grok 向官方聊天代理发送 `Reply OK.`，输出上限 1 token，不附带工具或项目内容。该操作不能保证额度周期开始或重置，周期由服务端决定；仅更新显示请用 `r`。超时或失败后可能已经产生一次请求，先用 `r` 检查额度。Codex 也可执行 `mux codex accounts wake <id>`。

导入成功后自动高亮，按 p 或点击 Apply Codex 会先在线校验该账号，再应用当前高亮账号；校验失败不会替换当前登录。同一身份重新导入会更新凭据，不增加重复账号；即使目标目录中仍有同一账号的旧凭据，也不会在激活时覆盖刚导入的新副本。不同工作区分别保存。已撤销的 refresh token 无法通过切换恢复：先在 Codex 中重新登录，再导入新凭据。

切换完成后重启 Codex CLI / App 并打开新会话。账号页的 `[Configured]` 是 Mux 保存的目标账号，`[Local login]` 是当前 Codex 磁盘凭据身份；不一致时会明确提示；API 模式保留的登录文件不代表 API 请求使用 ChatGPT 订阅。本地身份读取不验证远程凭据有效性。

在 Codex 内切换模型或推理强度后，可以直接回 Mux 按 `p` 应用。连接地址等受管字段发生外部变化时，按 `s` 查看，必要时按 `D` 断开后再应用；事务恢复使用 `mux codex recover`。

```sh
mux codex accounts import --name current
mux codex accounts import --name another --file /absolute/path/auth.json
mux codex accounts list
mux codex accounts check <account-id> # 在线校验保存的登录，不切换
mux codex accounts use <account-id>
```

### Codex 文件与恢复

- Codex 目标目录遵循 `CODEX_HOME`，默认 `~/.codex`。登录与额度查询调用 `codex`，可用 `MUX_CODEX_BIN` 指定二进制路径。
- 保存的账号元数据位于 Mux 配置；凭据副本位于 Mux 状态目录的 `codex-accounts/<id>/auth.json`。Unix 下目录为 `0700`、文件为 `0600`。这些文件包含登录凭据，不应提交或分享。
- 当前 Codex 登录遵循其 `cli_auth_credentials_store`：支持文件、系统凭据库及 `auto`。`ephemeral` 登录不能持久化切换。系统凭据库访问失败会报告错误。
- 保留 Codex 配置注释、MCP、权限、插件及其他非受管字段。项目配置、启动参数和认证环境变量仍可能覆盖用户级设置。
- 写入使用锁、原子替换、受管字段比较和事务日志。中断后执行 `mux codex recover`；外部修改冲突不会静默覆盖。
- `mux codex disconnect` 恢复仍属于 Mux 的配置字段，保留外部编辑。卸载会预览并清理登记过的账号文件，恢复受管 Codex 配置，保留聊天记录。
- 首次保存后 Mux 配置升级到版本 4；旧版本不识别该版本，升级前可自行保留配置备份。

## 模型状态规则

Mux 将“模型存在”和“模型启用”分开处理：

- 添加或发现模型会把它放入目录；未启用时模型仍然存在。
- `Space` 只切换启用状态，不删除目录项。
- 模型列表中的 `x` 和删除按钮统一删除已配置模型，不区分模型来源，并要求确认。删除会清理角色、子代理和回退引用；默认模型由剩余模型接替。刷新接口目录不会自动恢复已删除模型，可通过 Add model 重新添加。最后一个默认模型需先添加替代模型才能删除。
- `disabled_models` 记录显式禁用项，因此重启后不会被默认模型或角色引用意外重新启用。
- `model-a` 与 `model-a[1m]` 是同一个目录模型；`[1m]` 只表示上下文规格，导入和发现时不会生成重复项。

## 模型 Token 参数

通过 `a`（Add model）添加模型后，在 Provider 页面选中该模型，按 `e` 可重新编辑模型 ID、名称、描述、启用状态、1M 标记和 Token 参数，按 `Ctrl+S` 保存。无论焦点位于模型列表还是详情面板，`e` 都编辑当前模型；`E`（`Shift+e`）编辑厂商配置。Home 厂商列表中，`e` 编辑选中的厂商。

在添加或编辑模型窗口内，按 `Alt+1` 可直接切换 `1M context`，无需移动到开关字段；普通数字 `1` 仍用于输入。Provider 页面非搜索状态下使用 `1` 切换。可配置以下参数：

- `Max output tokens`：最大输出上限。留空不限制；填写 `8192` 时，Claude 请求 `16384` 会下调到 `8192`，请求 `4096` 保持不变；请求没有提供上限时使用此值。
- `Context window`：上下文容量记录，用于展示和检查最大输出不超过容量。不裁剪对话，也不改变 Claude 自动压缩行为；`1M context` 仍是独立的模型标记。

两个字段只接受正整数。例如模型配置可以包含 `max_output_tokens = 8192`、`context_window = 32768`。已接入的配置保存后自动同步；限制由代理在解析出实际模型后应用，覆盖 Anthropic、Chat Completions 和 Responses。若上限与请求的 thinking budget 冲突，代理报错而不会擅自更改推理参数。

`1M context` 为模型 ID 添加 `[1m]` 标记，不会提升上游模型的实际容量；请按服务商支持情况启用。

## Claude `/model` 同步

Mux 不启动 Claude，也不接管 Claude 的会话参数。同步采用“首次手动接入，之后自动更新”：

- 首次编辑只保存 Mux 配置。按 `p` 或执行 `mux apply --profile <id>` 成功后，建立与当前 Claude settings 文件的接入记录。
- 接入后，在任何页面修改厂商、模型启用状态、默认模型、角色或上下文规格，都会自动同步；连续修改会合并到最新状态。
- 自动同步沿用上次明确选择的默认厂商，不随浏览位置变化。厂商或默认模型不可用时，选择可用项；全部禁用时清空 Mux 管理的模型，保留接入记录。
- 写入 Claude 前会备份 settings，并保留无关配置。同步失败时，本地修改仍然保存，按 `p` 重试；重新打开 TUI 时会检查未同步修改。
- 同步记录上次写入的受管字段快照。如果地址、Token、默认模型或其他受管字段被手动修改或被其他工具修改，自动同步暂停。确认需要 Mux 重新管理后，按 `p` 重新接入。
- 没有快照的旧连接升级后需按 `p` 一次建立快照。

底部状态为 `Not connected`、`Pending`、`Syncing`、`Synced`、`Failed` 或 `Paused`。接入记录绑定 Mux 配置路径和 Claude settings 路径，并在重启后保留。

同步完成后，从普通终端运行 `claude`，再使用原生 `/model` 选择 Mux 管理的模型。在 `/model` 中按 `Enter` 可能写入 Claude 的全局默认模型；只想修改当前会话时按 `s`。


## Claude Code 客户端设置

点击 **Settings** 或按 **F4** 打开分类页，选择 **Claude** 进入客户端设置；在 Claude 标签页的主题设置页也可按 `c` 进入。这些设置对当前 Mux 配置的所有 Claude Provider 共用，切换模型仍使用各自的地址、认证和协议，同时保留客户端设置。它们不会修改系统或 shell 环境变量，也不影响 Codex / Pi。

Claude / Codex / Pi / Grok 的 Provider 页面使用与 Usage 一致的整体边框，Provider 导航、模型、连接详情和状态信息都位于框内。操作栏统一位于右上角，使用 `Apply [p]`、`Help [?]` 等文字和弱化的快捷键；页面操作栏的 Help 与 Back/Quit 位于同一行；窄窗口自动换行，底部保留状态信息。选中的模型和详情面板使用当前主题的选中样式；`h/l` 逐层返回 / 进入，`q` 返回一层，模型搜索输入时保留文字输入行为。

All Models 支持 `/` 或点击 **Filter** 输入模型名称、ID、Provider 名称或 ID；不区分大小写，多个关键词同时匹配，并显示筛选结果 / 全部模型数量。`Enter` 结束输入，`Esc` 或 **Clear** 清空筛选。全屏下单击模型在右侧显示模型配置（所属 Provider、API、地址、默认/启用状态、1M、上下文、输出上限和模型说明），重复点击不跳页；`Enter` 进入所属 Provider。`Tab` 切换导航、列表和配置面板，配置面板支持滚轮/方向键滚动；半屏仍使用原来的进入 Provider 操作。筛选后的启用、应用和设置默认操作均对应当前可见的选中模型。

Provider 页面根据终端尺寸自动切换：至少 **120 列 × 24 行**时，左侧常驻紧凑 Provider 导航，右侧显示模型搜索、模型操作和连接详情；选择 Provider 即时更新右侧。点击左侧或在左侧用方向键切换，`Tab` 循环切换导航、模型和详情，`Esc` 返回导航，`a` 新增 Provider。小于此尺寸时保留原来的 Provider 首页和点击进入第二层的操作。窗口缩放保留当前 Provider，模型仍可在原详情页中管理。

TUI 主题统一控制整个界面的画布、面板、边框与文字层级，覆盖所有客户端、模型表单、账号页、弹窗、Usage 和独立 Pulse 侧栏。标题与关键模型值加粗，字段标签、厂商 ID 与辅助信息使用次级文字色；选中行保留每段文字的颜色和字重。彩色主题的默认模型、启用数量、连接成功、警告和错误分别配色；Classic 保留 0.1.18 的默认模型警告色和启用状态成功色。终端字体家族由终端设置决定。

| 主题 | 默认模型文字 | 启用状态 / 次级读数 | 标题与边框 |
| --- | --- | --- | --- |
| **Graphite（石墨）** | 冰蓝 | 淡紫 | 象牙白标题、简洁竖边 |
| **Tundra（苔原）** | 鼠尾草绿 | 铜色 | 羊皮纸色标题、粗边框 |
| **Paper（纸页）** | 墨蓝 | 棕色 | 深棕标题、细框、下划线选中态 |
| **Nightfall（夜航）** | 淡紫 | 海玻璃色 | 淡玫瑰色标题、圆角框 |
| **Pulse** | 青蓝 | 金色 | 亮白标题、双线框 |
| **Classic（默认）** | 0.1.18 原警告色 | 0.1.18 原成功色 | 加粗标题、终端背景与传统方框 |
| **Arctic（冰蓝）** | 淡紫 | 薄荷色 | 冰白标题、蜜桃色标签、冰蓝方框与箭头 |
| **Ember（暖铜）** | 天蓝 | 蜂蜜色 | 暖白标题、鼠尾草绿标签、铜色方框与箭头 |
| **Orchid（兰紫）** | 海玻璃色 | 玫瑰色 | 淡紫白标题、粉蓝标签、紫色方框与箭头 |

Arctic、Ember、Orchid 仅改变文字和方框配色，始终沿用终端背景；面板、选中行、标签页和按钮均不填充背景色。名称、字段标签、默认模型、启用数量和辅助文字搭配不同颜色；选中项通过加粗实心 `▶` 标识，标签页和按钮保留局部下划线。主界面与 Pulse 侧栏均可在 F4 中选择这些主题。默认主题为 Classic，恢复 0.1.18 的终端配色。仅 Arctic、Ember、Orchid 在厂商、模型、API 模型、账号、主题列表和 Usage 表格中使用选择箭头；箭头选中行不加下划线，原有主题继续使用原来的选中背景；启用、默认和账号选择状态仍使用各自的圆点或菱形标记。

已有主题 ID 和选择继续有效；Pulse 侧栏中的会话及输出读数随主题变化，真正的连接、成功、警告和错误保留独立语义。F4 预览使用真实厂商摘要和 Pulse 读数组件，可直接比较文字配色。

按 **F4** 打开 Settings 工作区。宽屏左侧选择分类，右侧直接修改；窄屏先选择分类，再按 `Enter` 编辑。`Tab` 切换分类与编辑焦点，`↑↓` 选择，`←→` 修改，`[` / `]` 切换分类并保留草稿。`Ctrl+S` 或底部 Save 保存且停留当前页，`Ctrl+R` 丢弃全部未保存修改，`Esc` 返回分类再退出；未保存时会提示，避免误切标签丢失修改。Display 提供即时主题预览；Pulse 同页设置主题、文字/图形视图、模型明细、首次页面和 Sessions 排序。保存后已打开的 Pulse 更新显示偏好，首次页面在下次打开时生效。Usage 刷新间隔支持 **1–60 秒**，默认为 **2 秒**。Claude、Grok 和 Proxy 的设置直接显示在工作区右侧，窄屏时占满内容页；保存或返回后回到分类列表，并保留其他分类的草稿。

New models 可分别设置 Claude、Codex、Grok 新建模型时是否默认启用；Claude 另有默认勾选 1M。它们只决定新建表单初始值，不改动已有模型。Claude、Codex、Grok 分类可从任意客户端页面进入对应设置，保存时不切换当前客户端；Claude/Grok 沿用原有原生配置编辑与同步规则，Codex 推理强度保存后仍按 `p` 应用。切换工作区分类、进入客户端设置再返回时均保留草稿；未保存的设置标为 unsaved。

主界面和 Pulse 主题分别保存在状态目录的 `tui-theme.json` 和 `pulse-theme.json`；刷新间隔保存在主配置的 `usage_refresh_secs`，Pulse 显示与新建模型默认值保存在 `[ui]`。旧配置缺少 `[ui]` 时保持原有默认行为。下次启动自动恢复，运行中的 Pulse 会自动读取主题和显示偏好。保存外观与 UI 默认值不会触发代理同步。

- 六项预设：AI 署名、Teammates、Tool Search、思考强度、禁用自动升级、禁用 Artifact。默认 `inherit` 表示不覆盖已有配置。
- 点击 **Fill presets**（窄屏显示 **Presets**），或按 **Alt+P**，填入隐藏署名、开启 Teammates / Tool Search、`max` 思考、禁用自动升级和 Artifact；这只修改草稿。
- **Add variable / Alt+N** 添加自定义变量；选中条目后 **Delete / Alt+D** 删除，**Show / Alt+V** 切换值的遮罩。值按原样保存，不执行 `$()` 或其他 shell 表达式。
- **Ctrl+S / Save** 保存。已连接时自动同步，未连接时等待按 `p`；**Esc** 返回，有改动时按 `y` 丢弃，其他键继续编辑。
- **Disconnect / Alt+X** 断开管理，恢复接管前的设置。删除覆盖或改回 `inherit` 也会在下一次同步时恢复原值；外部修改不会被恢复操作覆盖。

存储在 Mux 配置的 `[claude]` / `[claude.env]` 下。同步目标遵循 `CLAUDE_CONFIG_DIR`，默认 `~/.claude/settings.json`。地址、认证、模型和配置目录变量由转发管理，不能作为自定义变量重复覆盖。文件使用私有权限，自定义值不会进入同步日志。

```toml
[claude]
hide_attribution = true

[claude.env]
CLAUDE_CODE_EFFORT_LEVEL = "max"
ENABLE_TOOL_SEARCH = "true"
CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS = "1"
DISABLE_AUTOUPDATER = "1"
CLAUDE_CODE_DISABLE_ARTIFACT = "1"
```

**同步成功表示配置已写入，不代表每个上游都支持对应功能。** 启动时读取的选项以及删除环境变量，需要重启 Claude Code。项目或组织层配置也可能影响实际生效值。

Anthropic 转发保留原生 Tool Search 内容。OpenAI Chat / Responses 使用普通函数调用兼容 Claude 客户端执行的搜索：传入当前请求的完整工具目录，将搜索结果中的引用转换为工具名称，保留调用 ID。这个模式不保证延迟加载的上下文节省；引用不在请求目录中或使用 Anthropic 服务端搜索工具时，返回明确错误。

模型编辑页新增 **Reasoning max**：`off / low / medium / high / xhigh`，默认 `high`，配置字段为 `reasoning_max`。转到 OpenAI 时，`max` 映射为该模型的上限，其他等级超过上限时下调，`off` 不发送推理参数。Chat 使用 `reasoning_effort`，Responses 使用 `reasoning.effort`；Anthropic 不改变原始等级。上游拒绝参数时不会暗中降级重试。

配置版本升级到 5；旧配置加载后默认不接管任何客户端偏好。升级后请勿使用只支持旧配置版本的 Mux 写回该文件。

Codex Account 的 **Reset [R]** 使用账号获得的额度重置卡。先刷新所选账号，选择最早到期且可用的 Codex 重置卡，再显示账号名称、卡片标题和有效期。按 Enter / y 或点击 Confirm 才兑换；Esc / n 或 Cancel 取消。确认时会消耗一张卡，无法撤销。没有卡、卡片详情不可用或旧版 Codex 不支持时会显示错误。兑换使用隔离登录副本，不切换当前账号；失败或超时不自动重试，请先 Refresh 检查额度与卡片状态。成功兑换但刷新失败时会明确提示已兑换，避免重复使用。协议见 [官方 App Server 文档](https://learn.chatgpt.com/docs/app-server#8-earned-rate-limit-resets-chatgpt)。
