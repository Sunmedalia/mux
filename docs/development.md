# 开发与验证

[返回 Mux 概览](../README.md)

## 开发

```sh
cargo fmt -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features -- --test-threads=1
cargo build --locked --release

# 可选：十万/百万条统计记录的查询与无变化刷新基准（使用临时数据库）
cargo test --locked --bin mux large_ledger_query_benchmark -- --ignored --nocapture
```

CI 在 main/dev 分支推送、Pull Request、版本标签推送或手动触发时运行：Ubuntu、macOS 与 Windows 执行检查，Windows 额外验证 Rust 1.88、npm 启动器、MSVC 静态运行库及解压后的 ZIP。版本标签通过跨平台测试、依赖安全审计和 Docker 安全回归后生成 Release。Windows ZIP 附带 SHA-256 校验文件。

独立 Rust 测试按串行调度，避免子进程继承文件锁造成偶发竞争，以及 macOS SQLite WAL 测试并行关闭连接造成超时。每个测试内部的并发请求、读写和冲突检查仍照常执行。
统计数据库测试与代理一样复用同一 Writer 及其共享连接，保留并发 Ticket 的最终写入验证，避免异步写入期间反复创建、关闭独立 WAL 写连接。

Codex 的自动测试使用隔离 HOME、模拟登录凭据和本地上游，不读取真实账号。可另行安装 Codex CLI 后运行真实进程冒烟测试（无 API 调用费用）：

```sh
cargo build --locked
SMOKE_FORMAT=openai-responses python3 tests/fixtures/codex_cli_smoke.py
SMOKE_FORMAT=anthropic python3 tests/fixtures/codex_cli_smoke.py
SMOKE_FORMAT=openai-chat python3 tests/fixtures/codex_cli_smoke.py
```

该脚本通过本地模拟上游驱动 Codex 执行固定的临时文件读取、修改与验证命令。已在 Codex CLI `0.153.4` 验证三种路径；ChatGPT App `26.901.41600` 内置后端已验证可读取生成的配置，App UI 登录切换与真实订阅额度仍需实际账号验收。

TUI 按状态、事件、页面、表单、模型规则、布局和后台任务拆分在 `src/tui/`；CLI 与 TUI 共用 `src/sync.rs` 的同步服务。回归测试包含真实事件序列、延迟本地 API、同步失败恢复、并发编辑及 120×36 到 40×12 的布局检查。

### Docker 安全回归

```sh
docker build -f tests/docker/Dockerfile -t mux-uninstall-safety .
docker run --rm --network none --read-only --tmpfs /tmp:rw,nosuid,nodev,exec mux-uninstall-safety
docker run --rm --network none --read-only --tmpfs /tmp:rw,nosuid,nodev,exec --user 10001:10001 mux-uninstall-safety
```

镜像内先执行 Rustfmt、Clippy 和 Rust 测试；卸载场景在独立临时 HOME 中运行，不挂载宿主机 HOME，也不挂载 Docker socket。检查覆盖预览、完整清理、重复卸载、中文路径、文件/目录链接、损坏文件、锁竞争、自启失败、伪造 PID、其他代理存活及无关文件内容不变。Linux Docker 测试不替代 Windows/macOS 自启管理器实机验证，也不构成对恶意同权限进程并发篡改或硬件故障的绝对保证。

详细覆盖范围和实测平台见 [Docker 测试记录](../tests/docker/RESULTS.md)。

### 模型最小测试

在模型详情页点击 **F5 Test model**，或按 **F5**，向所选模型所属的 Provider 发送一次简短的 `Reply OK.` 请求。支持 Anthropic、OpenAI Chat 和 Responses，最多请求 64 个输出 token，30 秒超时。收到实际模型输出（包括推理输出）即通过，不要求必须回复 OK。状态栏显示模型名称、响应耗时或失败原因；HTTP 成功但没有输出不会被判定为通过。此测试仅验证基础文本推理，不覆盖 Codex 的工具调用、流式响应、推理参数或已有对话兼容性。

测试在后台执行，不改变模型选择、Provider 配置或同步状态；测试的是上游模型响应，不依赖本地转发是否启动。环境变量配置继续从底部 **Settings** 或 **F4** 进入，主页面右上角不再单独放置入口。

新增或编辑 Provider 时，每个模型输入框末尾提供 `[Test]` 和 `[1m]`。Test（或选中该行按 F5）使用草稿中的 URL、认证与模型名发送最小请求，不要求先保存 Provider；Fallbacks 行依次测试全部填写的模型。测试结果显示在表单底部。`[1m]` 高亮表示启用，灰色表示未启用，仍可用 Alt+1 切换。

Base URL 行也提供 `[Test]`（选中该行按 F5）。使用草稿地址和认证发送一次 GET 请求，8 秒超时，不调用模型。结果区分网络连接失败和 HTTP 状态：401/403 表示服务器可达但认证被拒绝，404/405 表示地址可达但基础路径不提供 GET 接口；需要确认模型可用时再使用模型行的 Test。

Codex 账号页支持 `Browser (b)` 浏览器登录和 `Device (d)` 设备码登录。输入账号名称后开始登录；等待时按 `Esc` 或 Back 取消。登录成功后账号自动保存并高亮，按 Space 选中、p 应用。网页登录使用本机 Codex 客户端和独立临时目录，不覆盖当前登录；Import / File 仍可导入已有凭据。

Codex 账号页的 `Rename (e)` 可修改当前高亮账号的显示名称；邮箱、工作区、套餐和登录凭据来自账号身份，不支持手动修改。Import 与 File 使用统一的可选账号备注，留空默认为 ChatGPT；File 先输入文件路径，再填写备注。重命名不会切换账号或重新登录。

选中账号后按 `x` 或点击 `Delete (x)` 可删除保存的账号和凭据，操作前会确认。已应用的账号需先切换或断开；删除保存记录不会退出 Codex 当前的本机登录。

Codex 账号详情显示缓存额度使用率、用量窗口重置倒计时、最近成功刷新时间，以及已应用/本机登录状态。点击 `Refresh (r)` 主动查询，平时浏览不会请求额度接口；`PgUp/PgDn` 滚动详情。查询失败保留旧缓存并标记失败，不把网络错误直接判定为登录过期。


## 优化验证与基准

```sh
# 本机最低 Rust 版本检查
cargo +1.88.0 check --locked --all-targets --all-features
# 本地 tokenizer 基准：比较重建与缓存，验证 Token 结果一致
cargo test --locked --bin mux tokenizer_benchmark -- --ignored --nocapture
# SSE 边界解析基准：比较逐字节与批量处理，验证事件内容一致
cargo test --locked --bin mux sse_decoder_benchmark -- --ignored --nocapture
```

基准使用本地短文本和长上下文，预热后分别取七次中位值，不请求真实厂商。短文本缓存路径目标至少提升五倍，结果见 [优化基准记录](optimization-results.md)。性能数字不作为跨机器 CI 的硬阈值。

回归测试覆盖错误脱敏、Pi 全局设置基准、后台 panic 隔离恢复、部分保存、资源范围与并发合并、正文解析前认证、满容量控制接口、流式名额释放、Token 工作容量和限流头转发。普通测试不操作真实用户自启服务。

大型模块分别组织为代理生命周期、平台服务、认证路由、协议转换、资源限制；TUI 事件按导航、鼠标与表单处理组织；Pulse 将焦点识别、数据采集、状态、组件和渲染分离。行为变更与代码移动可分别审查。
