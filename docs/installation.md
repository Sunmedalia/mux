# 安装

[返回 Mux 概览](../README.md)

## 安装

v0.1.19 起，发布包、命令与环境变量统一为 Mux。更名前的历史 Release 保留原样；更新时请使用 v0.1.19 或更新版本。

如果你要安装的是 **Herdr 侧栏插件**，直接看 [Herdr 插件安装](pulse.md#通过-herdr-安装发布版插件)，不需要先手工安装 Mux；支持自动绑定的版本会配置默认快捷键。

### 一键安装（macOS / Linux）

使用以下命令安装最新发布版，自动校验 SHA-256：

```sh
curl -fsSL https://raw.githubusercontent.com/Sunmedalia/mux/main/install.sh | bash
```

安装 **Herdr 的 Mux Pulse 插件**，请在 Herdr 普通终端中执行（只需已安装 Herdr 0.7.0+，不需要 Git、Rust 或 Cargo）：

```sh
curl -fsSL https://raw.githubusercontent.com/Sunmedalia/mux/main/install.sh | bash -s -- herdr
# 自定义快捷键
curl -fsSL https://raw.githubusercontent.com/Sunmedalia/mux/main/install.sh | bash -s -- herdr --key prefix+shift+u
```

Mux 安装到 `~/.local/bin/mux`，覆盖前备份旧程序；若目录不在 PATH，脚本会提示添加方式。支持 macOS ARM64、Linux x86_64 / ARM64。
Herdr 模式先检查 `herdr` 命令，不存在就提示“没有 Herdr”并退出；不会下载或安装 Herdr 本体。检测通过后下载并校验预编译 Mux，生成不含构建步骤的插件清单，安装到 `${XDG_DATA_HOME:-$HOME/.local/share}/mux/herdr/plugin.*`，链接插件并合并快捷键；请保留该目录。重复运行可更新，旧插件目录保留以便恢复。该模式也会安装 Mux 命令，需要发布版支持 `herdr-install`。

指定已发布的 Mux 版本时，设置 `MUX_VERSION` 为对应标签；历史标签尚不提供 Mux 安装包。

以上在线命令需要本脚本已合并到 GitHub 的 main 分支。本地源码安装方式见下文。

### 下载 Release

v0.1.20 提供 macOS Apple Silicon、Linux x86_64/ARM64 与 Windows x64 发布包。macOS/Linux 可使用下列命令下载，Windows 安装说明见 [README-Windows.md](../README-Windows.md)。

```sh
# macOS Apple Silicon
curl -L https://github.com/Sunmedalia/mux/releases/latest/download/mux-macos-arm64.tar.gz | tar -xz

# Linux x86_64
curl -L https://github.com/Sunmedalia/mux/releases/latest/download/mux-linux-x86_64.tar.gz | tar -xz

# Linux ARM64
curl -L https://github.com/Sunmedalia/mux/releases/latest/download/mux-linux-arm64.tar.gz | tar -xz

chmod +x mux
sudo install mux /usr/local/bin/mux
```

### Windows

下载 [v0.1.20 Windows x64 ZIP](https://github.com/Sunmedalia/mux/releases/download/v0.1.20/mux-windows-x86_64.zip) 及旁边的 SHA-256 文件。完整的校验、解压、PowerShell/CMD 示例、自启和更新方法见 [Windows 使用说明](../README-Windows.md)。

配置默认位于 `%APPDATA%\mux\config.toml`，状态与缓存位于 `%LOCALAPPDATA%\mux\state`、`cache`。关闭 TUI 不会停止后台代理；更新前先执行 `mux proxy stop`，移动程序前先卸载旧位置的自启项。

### 从源码安装

需要 Rust 1.88+：

```sh
git clone https://github.com/Sunmedalia/mux.git
cd mux
cargo install --path .
```

Mux 支持 macOS、Linux 与 Windows 10/11 x64，需要 Claude Code 2.1.242 或更高版本。
