# Herdr Pulse

[返回 Mux 概览](../README.md)

## Herdr Pulse 常驻监控

`mux quick` 是独立的监控页面，不加载配置编辑器，也不会在启动时同步配置。
在 Codex / Grok 页面按 `a`，或点击账号卡片中的**当前账号 ▾**，在卡片内展开账号下拉菜单，下面的额度、进度条和统计内容会顺势向下排列，快速选择已保存账号。菜单与后续内容共用侧栏滚动，收起后恢复原来的布局。
用 `↑↓` / `j k` 选择、`Enter` 进入确认，再按 `Enter` / `y` 执行切换；`Esc` 返回或取消。
鼠标点击账号后仍需点击 Confirm 才会切换；点击菜单外关闭下拉菜单。列表中的 `●` 表示本地当前登录。
切换异步执行，完成后自动刷新账号与额度；Grok 已运行的会话需要重启，Codex 会沿用完整编辑器的订阅配置同步和后台服务处理。
首次打开 Grok 账号列表会保存当前本地 OAuth 登录；新增、重命名、删除账号仍使用 `e` 打开的完整编辑器。
展示今日 Token、请求次数、缓存 Token、调用健康度、24 小时请求趋势，
以及服务商/模型的调用数和失败数。蓝灰底色、三行大号数字和独立的文字层级用于常驻侧栏；
字体家族继承终端，不修改其他 pane 的字体。面板使用完整高度，短屏可滚动；pane 小于 32 × 12 时自动切换为迷你布局，保留客户端切换、关键统计、滚动和常用操作。
TOKEN 页的顶部页签、客户端筛选和内容连续排列，状态提示直接位于底部按钮上方；账号进度条、重置时间与各模块之间不插入额外空行，高窗口也不会增加间距。可视化账号卡在宽度足够时将进度条、百分比和重置时间放在同一行，窄屏保留重置时间下一行。TOKENS 标题与 CODEX / GROK ACCOUNT 使用一致的粗体和分隔线，日期保留在右侧；窄屏优先显示标题。

### 通过 Herdr 安装发布版插件

```sh
herdr plugin install Sunmedalia/mux
```

Herdr 下载仓库后，安装钩子会按 `herdr-plugin.toml` 的版本下载对应 Release 的预编译 Mux，校验 SHA-256 和程序版本，再放入插件目录的 `target/release/mux`。支持自动绑定默认快捷键 `prefix+u`：已有 Mux 快捷键会保留，键位冲突会提示并跳过，修改配置前会备份。无需 Rust / Cargo，不在用户机器上编译，也不安装全局 `mux` 命令。需要 Git（Herdr 下载仓库）、Bash、curl、tar 和 sha256sum 或 shasum；支持 macOS ARM64、Linux x86_64 / ARM64。对应版本的 Release 必须已发布，下载失败会中止安装。

安装后在 Herdr 内打开侧栏：

```sh
herdr plugin action invoke mux.open
```

自动绑定需发布包含此改动的 Mux 二进制；旧版 Release 会提示手动配置。更新时重新运行安装命令；如果之前使用本地脚本链接过同名插件，先执行 `herdr plugin unlink mux`，再安装。

此方式需包含下载钩子的提交已发布到仓库默认分支；旧 tag 仍使用该 tag 自己的安装流程。

### Herdr 插件一键安装（本地源码开发）

支持 macOS / Linux，需要支持 `herdr plugin` 的 Herdr（0.7.0+）和 Rust 1.88+ / Cargo。**在 Herdr 的普通终端中**，进入本项目目录，执行：

```sh
bash scripts/install-herdr.sh
```

这一条命令会依次：

- 构建当前源码的 release 程序，包括尚未发布的本地修改。
- 更新 `~/.local/bin/mux`，替换前备份旧程序，不需要 sudo。
- 把 Herdr 的 `mux` 插件链接到**当前项目目录**并启用，替换之前链接的旧目录。
- 自动合并 `prefix+u` 快捷键并重载 Herdr 配置；已有的 Mux 快捷键会保留，不重复添加。配置改动前会输出备份位置，其他插件和快捷键保持原样。

安装完成后，默认按 **Ctrl+B，再按 u** 打开 / 关闭侧栏。侧栏直接按 **`s` / Sessions** 查看会话用量；按 `s` 返回今日用量。也可以按 `e` 打开完整 Mux，在 **Usage → `6` Sessions** 查看更详细的列表。如果你修改过 Herdr 的 prefix，使用自己的 prefix。

项目目录是插件的运行位置，安装后请保留它。**更新时，在同一目录更新源码，再运行同一条安装命令即可。** 已打开的 Mux 窗口仍是旧进程，需要退出后重新打开。脚本不会关闭工作中的 agent、重启 Herdr，或自动中断代理请求。

尚未下载源码时，先执行 `git clone https://github.com/Sunmedalia/mux.git && cd mux`，然后运行上面的脚本。脚本安装的是当前 checkout；旧 Release tag 可能尚未包含该脚本及 Sessions 功能。

如果 `prefix+u` 已被其他功能占用，安装器会说明冲突，不覆盖它；改用一个空闲键：

```sh
bash scripts/install-herdr.sh --key prefix+shift+u
```

**更新缓存 / 速度采集逻辑后**，旧代理也需要更新。在没有正在进行的 API 请求时执行（如果代理本来没启动，只执行 start）：

```sh
"$HOME/.local/bin/mux" proxy stop
"$HOME/.local/bin/mux" proxy start
```

代理保留已有监听地址和路由；不会同步或改写 Claude / Codex 的模型配置。新指标从新版代理采集的新请求开始显示。

确认安装结果：

```sh
herdr plugin list --plugin mux
"$HOME/.local/bin/mux" --version
```

### 使用侧栏

快捷键在当前标签页最右边界的 pane 右侧打开常驻监控，保留原 pane 的焦点；
再次触发会关闭当前标签页已有的监控，再按则重新打开。也可执行 `mux quick --open` 切换开关。
首次打开时按触发快捷键的 pane 自动选择统计页：Codex → Codex，Claude Code → Claude，
Grok → Grok；未识别到这些 agent → All。打开后通过 Herdr 焦点事件跟随同一标签页当前聚焦的 Claude / Codex / Grok pane，Gateway、图表和 Sessions 均切换到对应客户端；事件连接不可用时每两秒检查一次。聚焦侧栏或普通终端时保留上一次 agent。手动选择 Claude / Codex / Grok / All 后，下一次切换 agent 焦点时恢复自动跟随。
直接运行 `mux quick` 默认展示 All。

- `Tab` 或 `1/2/3/4` 切换 Claude / Codex / Grok / 全部统计，鼠标点击同样可用。
- 每 2 秒检查本地用量库；数据库版本未变化时跳过统计查询。`r` 立即检查。读取失败保留旧数据并标记 STALE。
- 首页在 `PROVIDERS / TODAY` 标题右侧点击 `[Models m]`（或按 `m`）切换服务商与模型明细；不再使用 `d`。滚轮、方向键、PgUp/PgDn、鼠标点击或拖动滚动条均可滚动，Esc/Home 回到顶部。
- **Codex** 页面顶部用紧凑账号卡显示名称、邮箱、套餐、本地登录状态及额度进度条。简洁版以紧凑额度条显示已用比例与相对重置时间，登录状态、账号数量和缓存年龄合并为一行；文字版保留完整字段。账号元数据每两秒更新；`r` 可在线刷新额度，Pulse 不切换账号。
- **Grok** 页面顶部先显示账号卡、额度进度条和模型列表，再以独立的 Gateway token 区域显示今日**经过 Mux 本地代理**的 Grok 请求用量、输入/输出及缓存；Session token 区域显示 Grok 原生 `updates.jsonl` 中当前会话累计的 token、缓存读写和命中率，两者不相加。选择 API Provider 并重启 Grok 后，新请求才会记入 Gateway token；OAuth 和未通过 Mux 代理的旧直连请求不会计入该区域。账号卡沿用 Codex 的布局：名称、邮箱和套餐、额度条、重置时间，以及本地登录 / 保存账号数 / 刷新年龄。简洁版将未知额度显示为同宽度的纹理条和 `—`，保留重置时间；预付余额、按需用量和模型配置放在文字版详情中。当前选择 Grok 时，每分钟自动刷新在线额度，`r` 立即刷新；失败保留同一账号缓存。`v` 在文字版与简洁版之间切换：文字版沿用 Claude 的完整网关指标、请求健康及服务商/模型明细，简洁版与 Codex、Claude 共用精细比例条。两版都显示网关输出速率（E2E）；Session 区域另显示原生日志的 API rate（有耗时记录的输出 token ÷ API 总耗时），不等同于纯模型解码速度，缺少有效耗时时显示 `—`。聚焦的 Grok session 优先；没有聚焦 Grok session 时显示最近一个有 token 的会话并标为 Recent；尚未写入 usage 时显示 `—`。
- Claude / All 侧栏首页首屏先显示**今日 Mux 网关用量**，再显示当前聚焦的 Claude / Codex pane 的**当前 session 累计 token**；两者均用大数字展示，互不相加，各自保留输入/输出、缓存读写和缓存率。session 缓存复用率 = 缓存读取 ÷ 总输入（不含输出；缓存写入不算命中）。当前会话来自本地日志；文件变化会触发更新，并每 30 秒兜底检查一次。文件监听不可用时改为每 2 秒检查。Codex 恢复同一 session 时会合并多份日志的累计计数，避免新日志尚未写入 token 事件时用量暂时消失。`s` / `Sessions` 打开会话页，顶部保留当前会话摘要，下方显示其他本地会话；沿用 Claude / Codex / All 筛选。焦点切换到另一 agent pane 或 agent 切换 session 时，侧栏随之切换对应客户端和当前会话。若 Herdr 尚未提供 session ID，新版 Codex CLI 可按终端显示的会话名称和项目路径，从本地 `state_*.sqlite` 只读匹配唯一的未归档会话，以兼容共享 app-server 的 hook 上报到旧 pane 的情况；名称重复、缺少标题或数据库不兼容时仍显示等待识别，不会把最近的日志误标为当前会话。会话页按 `t` 切换最近活动 / token 排序，`r` 刷新，`?` 查看统计说明。Fork 会话标记 `*`，可能包含继承用量。
- `c` / `Chart` 切换首页与图表页：上方为今日网关每小时请求数，下方为当前 session 今日每小时 token 增量（输入+输出，来自本地日志）；两组图分别缩放，不应直接比较柱高。
- `v`（或点击右上角 `V(v)` / `T(v)`）在文字版和简洁图形版之间切换，当前页面、客户端筛选和排序保持不变。简洁版仍保留网关与当前 session 的大 Token 数字；网关指标改为输入/输出双色条、请求/未知计数、缓存命中条（内含 R/W 读写量）及速率/测量流数的紧凑读数，保留各项数值而减少重复标签。健康条按已完成请求分为绿色成功、红色失败、金色中断，待完成请求不计入比例。图表页原本就是图形展示，切换后图表数据不变。
- `e` / `↗ Edit` 新开 Herdr 标签页运行完整 Mux，并立即切换到新标签页和编辑 pane；监控 pane 继续常驻。
- 监控与 Edit 均由 Herdr 原生插件直接启动，终端不再显示 `exec` 或启动命令。
- `q` / `×` 退出监控。
- 顶部固定显示 TOKEN / GIT 互斥分段页签，`●` 标记当前页，`○` 标记另一页，点击当前页保持内容。Alt+1 / Alt+2 分别选择 TOKEN / GIT，`T` / `g` 也可直接选择；切回 TOKEN 保留之前的统计子页、筛选和滚动位置。底部操作按钮标注 `(e)`、`(c)`、`(s)`、`(r)`、`(q)`；会话页把 `(c)` 换为排序 `(t)`。
- 两组 24 小时趋势使用铺满内容宽度的六行柱状图，标出小时刻度；零用量不画虚假柱。
- `?` 或右上角帮助按钮查看统计范围，`Esc` 返回，首页不再常驻显示范围说明。

统计范围为当前 Mux 配置经过本地网关的请求，**不是当前 Claude 会话统计或订阅剩余额度**。
直连 API 和 ChatGPT/Claude 订阅流量不包含在内。成功率 = 成功 /（成功 + 失败 + 中断），
待完成请求不计入分母；没有已完成请求时显示“无样本”。请求计数包含单独标注的压缩请求，
Token 缺失时显示未知提示，不当作零用量。插件支持 macOS / Linux。

监控首页在 `TOKENS / TODAY` 大数字下紧接显示请求数、网关缓存读写、`Cache hit`、`Output rate (E2E)` 和测量流数，然后才进入当前 Session 区块。缓存命中率按有完整缓存计数的成功生成请求计算，
分母统一为包含缓存读取/写入的全部输入 Token（OpenAI 输入本身已包含缓存，Anthropic 需相加），
缓存写入不算命中。协议取实际路由的上游格式，OpenAI 兼容网关即使返回 Anthropic 风格缓存字段，也不重复加到输入分母；支持 DeepSeek `prompt_cache_hit_tokens`，cache miss 不当作缓存写入。缺失 Anthropic 缓存写入计数时不猜测完整分母。
端到端输出速率是今日成功流式生成请求的输出 Token 总数除以对应请求耗时总秒数，
从发出上游请求计到流完成，包含首 Token 等待、隐藏推理和网络传输时间，不等于模型纯解码速度。页面显示测量样本数；无有效样本时显示 `—`。
旧记录保留，不回填未知协议或计时，也不将旧版缓存分母和输出阶段计时混入新指标；新版网关启动后采集新请求，面板重开即可展示。

### Git 侧栏

顶部 **TOKEN / GIT** 是同一组互斥页签，只标记一个当前页。按 **Alt+2 / `g`** 或选择 **GIT** 打开 Git；按 **Alt+1 / `T`** 或选择 **TOKEN** 返回之前的用量子页。重复选择当前页保持内容，Git 的文件选择、Diff、Log 和搜索结果在切换后保留。两页共用相同的页签位置和样式，底部不显示重复的 Home 切换按钮。编辑表单、确认或执行命令时暂时禁用页面选择，避免丢失草稿。也可在完整 Mux 的 Settings → Pulse → Start page 选择 Git，之后打开 Pulse 默认进入 Git 页。所有 Git 操作都在侧栏内完成。

Git 文件列表顶部固定展示仓库概览：首行直接显示自动识别的仓库绝对路径，与当前分支选择按钮放在同一行；分支使用普通文字与下拉标记，不使用高亮背景。下方显示上游和领先 / 落后数，以及未暂存、已暂存和冲突数量，滚动文件时保持可见。下方分组标题带文件数，未暂存用金色、已暂存用绿色、冲突用红色。文件行展示状态字母、文件名和独立着色的增删行数，目录另起一行；长路径和重命名信息换行展示，点击续行仍选择对应文件。选中行有侧标和背景高亮，短屏使用紧凑布局。

Git 跟随同一 Herdr 标签页中最近聚焦的普通终端或 agent pane 的项目目录；聚焦 Pulse 时保持仓库。切换终端、切换目录后通过焦点事件更新，两秒轮询兜底。支持仓库子目录、独立 worktree；直接运行 `mux quick` 时使用启动目录。非 Git 目录显示空状态。打开操作菜单、编辑表单或执行命令期间固定仓库，确认界面标出仓库和分支。

顶部展示分支、上游和本地领先/落后的提交数，文件按 Conflicts、Unstaged、Staged 分组，展示状态与已跟踪文件的增删行数。部分暂存文件会出现在两个分组中。方向键或 `j/k` 选择文件；鼠标单击选择、再次点击打开，或按 Enter 进入 Diff。Diff 顶部固定显示文件名、当前文件序号和返回提示，滚动时仍可查看。按 `,` / `.` 或点击底部 Prev / Next 切换文件，`Esc` 或 Files 返回列表，保留当前文件选择。Diff 默认自动换行，按 `w` 或点击 Wrap ON/OFF 切换；关闭换行后用 `←/→` 横向滚动。续行使用 `↳` 标记，保留增删颜色和差异块归属，原始内容及补丁保持不变。`[/]` 切换选中差异块，点击原行或续行都可选择对应差异块；`r` 刷新当前状态或 Diff。状态日志在按钮上方，底部导航和操作分为两行，按钮标出快捷键，Stage all 一次暂存仓库全部更改，按当前状态显示 Unstage / Discard / Refresh；边界文件的 Prev / Next 和执行中的操作会置灰。

底部按钮和提示随当前页面更新：文件列表显示 Diff / Commit / Log / Branch，Diff 显示文件切换、换行和 Files 返回，Log 显示历史翻页、详情和搜索。详情页用 Log / Diff / Files 标出返回目标；创建或跟踪分支按 Enter / Review，提交消息按 Ctrl+Enter / Review，提交确认页用 Edit 返回草稿。搜索框有 Clear(Ctrl+U)，清空后显示 All commits(Enter)，提交才恢复完整历史。空表单、无匹配菜单项、滚动边界以及非 Git 目录中不可用的按钮置灰。

- `s` / Stage file：暂存当前选中的整个文件；Diff 页同样只暂存当前文件。暂存后的列表按钮显示 Unstage file(u)，文本 Diff 显示 Unstage block(u)；撤销按钮同样区分整个文件与选中差异块。
- `a` / Stage all：一次暂存仓库全部更改，包括新增、修改和删除文件。文件列表与 Diff 页行为一致。`u` 取消暂存当前文件或 Diff 中选中的差异块；单文件和差异块暂存仍可通过 `o` 菜单执行。
- `d`：确认后撤销未暂存文件或选中差异块；保留已暂存内容。未跟踪文件显示删除确认。Diff 中 `D` 操作整个文件。
- `A` / `U`：暂存 / 取消暂存全部文件；也可在 `o` 菜单选择整文件操作。
- `n`：提交已暂存内容。输入第一行标题，Enter 添加正文；Ctrl+Enter 或底部 Review 进入确认，再按 Enter / `y` 执行。确认页按 Esc 返回草稿，表单按 Esc 取消。未解决冲突、没有暂存内容或提交前暂存区发生变化时拒绝提交；保留 Git hooks 和签名检查。
- 点击仓库绝对路径旁的 **⑂ 分支名 ▾** 按钮或按 `b`：打开分支选择页，搜索本地及远端分支。选中本地分支后确认切换；选中 `remote:` 分支后输入本地跟踪分支名称。执行命令时暂时禁用分支选择，短屏仍可点击。`o` 菜单也可创建并切换新分支。
- `o`：完整操作菜单，包含 Fetch、Pull、Push。Fetch 选择远端；Pull 使用 `--ff-only` 且要求干净工作区；已有上游的 Push 仅推送当前分支至其上游，首次 Push 选择远端并设置上游。Pull/Push 执行前确认，不提供强制推送。
- `L` / Log：查看当前分支提交历史，每页 50 条，展示摘要、作者、日期和分支标签；`,` / `.` 或 Newer / Older 翻页。Enter 或 Details 查看完整提交信息、文件统计和 Diff，内容自动换行；Esc 返回选中的提交，再按 Esc 返回文件列表。`r` 刷新历史。`/` 或 Search 打开搜索框，按 Enter 搜索当前分支全部提交的标题和正文，使用不区分大小写的字面文本匹配；搜索结果仍按每页 50 条翻页。Ctrl+U 清空搜索框，再按 Enter 恢复完整历史；Esc 取消搜索，保留原结果。
- `?` 查看 Git 帮助；`l` 查看完整操作输出或错误，长表单、确认和详情用 PgUp/PgDn 滚动。

新增、删除、重命名、二进制、权限变更和子模块仅支持整文件操作；冲突在外部解决后回到侧栏暂存。差异块执行前核对文件和暂存区，内容变化时提示刷新重新选择。Git 在后台串行执行，操作结束刷新状态；认证沿用现有 credential helper 和 SSH 配置，禁用终端密码提示，120 秒超时会显示错误。需要交互登录时先在普通终端完成认证再重试。

本页不包含图形提交拓扑、Stash、Rebase、Cherry-pick、分支删除、图形冲突编辑或逐行暂存。Git 操作需要系统 `git` 命令。源码改动后运行安装脚本构建并更新插件，关闭旧 Pulse 后重新打开即可使用。

### 手动安装（可选）

推荐使用上面的一键脚本。需要自己管理程序和快捷键时，可以仅构建并链接源码：

```sh
cargo build --locked --release --bin mux
herdr plugin link "$PWD"
herdr plugin enable mux
```

当前 Herdr 的 `plugin link` 默认启用，不接受旧示例中的 `--enabled`。手动安装需自行将下面的绑定合并到 Herdr `config.toml`（遵循 `HERDR_CONFIG_PATH`，默认 `~/.config/herdr/config.toml`），然后执行 `herdr server reload-config`：

```toml
[[keys.command]]
key = "prefix+u"
type = "plugin_action"
command = "mux.open"
description = "Toggle Mux Pulse usage monitor"
```

固定版本安装应选择更名后的 Mux 标签；历史标签仍是旧插件。当前源码安装会自动绑定默认 `prefix+u` 快捷键；已有 Mux 快捷键会保留，冲突会提示并跳过。

点击 Provider 面板或 Model 面板即可切换添加对象：`a` 在 Provider 导航中创建 Provider，在模型列表或配置详情中创建 Model。两个列表标题右侧各有 `[+]`，直接添加对应对象；顶部仅显示当前面板对应的 Add 操作。All Models 中以选中的模型所属 Provider 为目标。

按 `?` 打开 Help（Usage 也支持），查看 Home、All Models、Provider、Forms、Usage、Accounts、Settings、Proxy、Pulse 全部 9 类快捷键。`1–9` 或点击分类直接跳转，`Tab` / `Shift+Tab` 切换，方向键、`PgUp` / `PgDn`、`Home` / `End` 滚动；窄窗口分类和说明自动换行。

Help 分类、顶部快捷操作和底部 Previous / Next / Back 均支持点击，弹窗会优先接收鼠标操作。`l` / `Enter` 打开当前分类对应页面，`h` / `Esc` 返回；Provider 导航中的 `l` 逐层进入模型和详情，`h` 逐层返回。Usage 的 `l` 等同 Enter，`h` 返回 Provider。输入框和搜索中仍可正常输入 h/j/k/l。

Provider 顶部与 Help 提供 **Disconnect [D]**：Claude 恢复接管前的设置，Codex 保留断开确认，Grok 恢复管理字段。Claude Preferences 原有 `Alt+X` 仍可使用；Pi 直接编辑本地文件，不提供 Disconnect。 Claude 断开后须完全退出并重新运行 `claude`，开启新会话；旧进程可能仍保留 `mux-role::` 等模型环境配置。使用 `--resume` / `--continue` 恢复的会话也可能沿用之前的模型选择。

主 TUI 顶部同一行显示居中的 Mux 和 Claude、Codex、Pi、Grok、Usage、Settings 标签；Pulse 插件顶部显示 TOKEN 与 Git 入口。滚动条按实际内容行数和视口比例显示，列表使用真实滚动偏移，滚到底时滑块到达轨道末端。Proxy 是 Settings 内的分类，进入后按 `Esc` / `q` 返回工作区。全屏时 Settings 与 Provider 的内容区域铺满终端宽度，外观预览随窗口扩展。Help 与 Back/Quit 在同一条页面操作栏，Back/Quit 位于最右侧：除根层外逐层返回，在 Provider 根层退出。Provider 工具栏不再显示 Models / Details；点击面板或按 `Tab` / `h` / `l` 切换。全屏选中 Codex / Grok 的 Account 时，左侧保留 Provider 列表，右侧复用账号页的账号列表、详情和完整操作按钮。窄窗口继续使用账号二级页。

Grok Account 的额度条与 Balance 分开显示。服务未发布使用比例时，保留 `— used` 纹理额度条与重置时间，显示 `Usage not published`；不会把余额当作额度或把未知用量显示成 0%。
