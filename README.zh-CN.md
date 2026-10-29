# cxa

[![CI](https://github.com/jievince/cxa/actions/workflows/ci.yml/badge.svg)](https://github.com/jievince/cxa/actions/workflows/ci.yml)

[English](README.md) | 简体中文

在 Codex CLI 和 Desktop 中切换 ChatGPT 订阅与兼容 Responses API 的中转服务。共用一个 Codex home，保留现有 `model_provider` ID，无需启动包装脚本或 `--profile`。

- ChatGPT 订阅：隔离登录、切换账号、查询剩余额度。
- API 中转服务：切换密钥、查询模型、显示可选的服务网站。
- CLIProxyAPI（CPA）：读取服务端提供的分配额度和重置时间。

本仓库基于 [jesse-merhi/cxa](https://github.com/jesse-merhi/cxa) 扩展。请从本仓库安装；上游发行版及 Homebrew tap 不包含这里的 API 中转功能。

## 安装

需要 macOS 或 Linux、可从 `PATH` 调用的 Codex CLI、稳定版 Rust，以及本机 C 编译器和链接器。

```sh
cargo install --locked --git https://github.com/jievince/cxa --branch main --bin cxa
```

更新：

```sh
cargo install --locked --force --git https://github.com/jievince/cxa --branch main --bin cxa
```

Cargo 默认安装到 `~/.cargo/bin`。请将其加入 `PATH`，用 `command -v cxa` 确认实际运行的版本，避免命中其他位置的旧程序。安装本身不切换账号，也不修改 Codex 配置。

## 开始使用

保存当前 ChatGPT 登录，无需退出或重新登录：

```sh
cxa init
```

添加 ChatGPT 订阅或 API 连接：

```sh
cxa add
```

ChatGPT 登录在隔离目录中进行。API 向导依次询问 base URL、API key、可选的服务网站、服务类型和可选名称。普通中转选择 `generic`，不查询额度；提供订阅查询接口的 CPA 服务选择 `cpa`。添加仅保存账号，不自动切换。

查看账号与额度：

```sh
cxa list
```

按名称切换，然后重启需要使用该账号的 Codex CLI 或 Desktop 进程：

```sh
cxa use company
```

两种类型都支持 `--name`。省略时默认使用邮箱前缀或 API 主机名。默认名称冲突时自动追加后缀；显式名称必须唯一，不区分大小写。名称长度为 1–64 个 ASCII 字符，可用字母、数字、`.`、`_`、`-`，以字母或数字开头，不能全为数字。

## 常用命令

| 命令 | 用途 |
| --- | --- |
| `cxa status` | 当前账号与凭据状态；直接运行 `cxa` 也显示此信息 |
| `cxa list` | 全部账号；`*` 表示当前选中项 |
| `cxa watch` | 持续刷新额度；按 `q` 或 Ctrl-C 退出 |
| `cxa use NAME` | 用完整名称切换账号 |
| `cxa rename OLD NEW` | 改名，不修改当前凭据或配置 |
| `cxa remove NAME` | 删除本地保存的账号；当前账号受保护 |
| `cxa models [NAME]` | 查询 API 服务的 `/models` |
| `cxa relogin NAME` | 重新登录 ChatGPT 账号 |
| `cxa import PATH` | 导入 ChatGPT 的 `auth.json` |

列表数字仅为从 1 开始的展示序号，不能用于操作。不支持 `cxa NAME` 简写。完整选项见 `cxa COMMAND --help`。

## 配置边界

cxa 不修改顶层 `model_provider` ID，也不编辑会话 rollout 或数据库。切换会修改当前 provider 的连接字段：订阅使用官方 OAuth 连接，API 使用对应的 URL 和密钥。模型、推理强度、MCP、项目及无关配置保留。

已有 `unicodex` 等自定义 provider 时，订阅模式的 `name` 写为 `OpenAI`，API 模式写为当前 cxa 账号名称。改名在下一次 `cxa use` 时同步到 provider。未设置自定义 provider 时继续使用内建 `openai`，通过 `openai_base_url` 接入中转。

认证要求 Codex 使用 `file` 存储。密钥以 `0600` 权限保存在本地，不加密。切换后，已有 Codex 进程需要重启。中转服务须支持 Responses API 和 `/models`；cxa 在切换前检查服务公布的模型，但无法保证已有聊天所选的模型一定可用。

精确的配置和认证修改、脚本添加、额度查询、环境变量及故障排查见[参考文档](docs/reference.zh-CN.md)。不保证未来所有 Codex 版本始终兼容。

## 开发

```sh
./scripts/check.sh
```

测试使用隔离目录和虚拟凭据。可选的真实 Codex 配置格式检查见[开发说明](docs/reference.zh-CN.md#开发验证)。

[MIT 许可证](LICENSE)。
