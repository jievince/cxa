# cxa

[![CI](https://github.com/jievince/cxa/actions/workflows/ci.yml/badge.svg)](https://github.com/jievince/cxa/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/jievince/cxa)](LICENSE)

[English](README.md) | 简体中文

`cxa` 用于在多个 ChatGPT 订阅账号和兼容 Responses API 的中转站 API key 之间切换。支持 Codex CLI 和 Codex Desktop，保留同一个 Codex home 和 `model_provider`，无需额外的启动器或 `--profile`。ChatGPT 和 CLIProxyAPI（CPA）账号支持额度查询，普通中转账号不查询额度，可以保存网站地址用于手动查看。

本仓库是 [jesse-merhi/cxa](https://github.com/jesse-merhi/cxa) 的 fork，增加了通用 API key 接入、CPA 额度查询、模型预检查和统一的剩余额度显示。安装本仓库的版本才能使用这些功能；上游 Homebrew 包和发布二进制不包含本 fork 的 API key 实现。仅支持兼容 Responses API 的服务，不支持只有 Chat Completions 接口的服务。

## 使用前确认

- 操作系统为 macOS 或 Linux，暂不支持 Windows。
- 已安装 [Codex CLI](https://developers.openai.com/codex/cli)，终端可以执行 `codex`。
- 使用 Codex 的文件型认证存储，默认位置为 `~/.codex/auth.json`。详见[认证存储](#认证存储)。
- 从源码安装需要当前稳定版 Rust 工具链和 `cargo`。
- 需要本机 C 编译器和链接器，例如 Linux 的 `build-essential` 或 macOS 的 Command Line Tools。
- API 服务提供兼容 OpenAI 的 Responses API 和 `/models`；CPA 额度查询还需要下文说明的订阅接口。
- API key 切换支持内建 `openai` 或已配置的自定义 provider，不转换 `ollama`、`lmstudio` 和 `amazon-bedrock`。

离线连接格式测试已在 Linux 上通过 Codex CLI `0.161.0` 验证，测试使用隔离目录和占位 key。这只说明 Codex 能读取两种连接配置，不代表所有网关和 Codex Desktop 版本都支持全部 Responses 功能。Codex 升级可能改变认证、配置或 App Server API，不能保证未来版本始终兼容。

## 安装和更新

安装本 fork 的 `main` 分支：

```sh
cargo install --locked --git https://github.com/jievince/cxa --branch main --bin cxa
```

Cargo 通常将程序安装到 `~/.cargo/bin`。确保该目录位于 `PATH` 中。如果此前通过 Homebrew 或 `~/.local/bin` 安装过 `cxa`，检查终端实际执行的程序，避免继续使用旧版本：

```sh
command -v cxa
```

更新已通过 Cargo 安装的版本：

```sh
cargo install --locked --force --git https://github.com/jievince/cxa --branch main --bin cxa
```

在本仓库的源码目录中安装：

```sh
cargo install --locked --path . --bin cxa
```

安装和更新只替换程序，不退出账号、不选择账号，也不替换已有账号目录。本 fork 的 CPA 功能目前通过源码分发；不要使用上游发布包安装本文介绍的功能。

## 快速开始

如果 Codex 已登录 ChatGPT，先保存当前登录：

```sh
cxa init
```

已有有效的文件型登录时，不需要先退出或重新登录。只有尚未登录时，才需要先执行 `codex login`。只有 CPA key、没有 ChatGPT 登录的环境，可以直接从添加 CPA 账号开始。

添加账号统一使用向导，先选择 ChatGPT 订阅或 API 中转：

```sh
cxa add
```

选择 ChatGPT 订阅后，再选择浏览器登录或设备码登录。选择 API 中转后，依次输入连接地址、key、可选网站和服务类型。两类流程最后都可以填写可选名称。也可以直接指定账号类型，跳过第一步选择：

```sh
cxa add --type chatgpt
```

无桌面开发机可以直接使用设备码登录：

```sh
cxa add --type chatgpt --device-auth
```

ChatGPT 订阅和 API 中转都支持可选的 `--name`：

```sh
cxa add --type chatgpt --name personal
```

```sh
cxa add --type api --name company
```

省略 `--name` 时，两类交互流程最后都会提示填写名称，回车接受默认值：ChatGPT 取邮箱的 `@` 前部分，API 取地址的主机名。非交互添加直接使用默认值。自动生成的名称重名时添加 `-2`、`-3` 等后缀；手动指定的名称必须在所有账号类型之间唯一，不区分大小写。在向导中输入非法或重名名称后可以重新填写，无需重复登录或输入 key。添加只保存账号，不自动切换。

ChatGPT 添加流程调用 `codex login`，在隔离的 Codex home 中完成 OAuth 登录，保存新账号后不会替换当前选中的登录，也不修改真实 `config.toml`。只保存已有登录使用 `cxa init`，无需重新登录。非交互添加订阅时，指定 `--type chatgpt` 使用 Codex 默认浏览器登录，或加 `--device-auth` 使用设备码。查询账号和剩余额度：

```sh
cxa list
```

按完整名称切换，名称不区分大小写：

```sh
cxa use personal
```

名称是唯一的账号标识，不再接受数字编号、完整邮箱或模糊匹配。旧账号首次使用时会保存固定名称：ChatGPT 账号默认取邮箱的 `@` 前部分，API 账号根据已有名称生成；重名时添加 `-2` 等后缀。删除其他账号不会改变名称，凭据文件保留在原目录。

列表显示从 1 开始的连续展示序号，删除账号后重新排列。序号仅用于阅读，切换、删除、改名和模型查询等操作仍只接受名称。

可以将名称改短：

```sh
cxa rename jaxonblazers jax
```

名称长度为 1–64 个字符，只允许 ASCII 字母、数字、点、下划线和连字符，首字符必须是字母或数字，不能是纯数字。名称不区分大小写且必须唯一。两类账号都可以通过向导或 `--name` 指定名称。

切换后重启实际使用的 Codex CLI 或 Desktop App Server，再照常启动 Codex。`cxa` 不自动重启进程；重启可能打断正在运行的任务，请先结束这些任务。使用远程开发机时，需要重启的是远端实际承载会话的 App Server，而不只是新建一个聊天窗口。

## 移除账号

ChatGPT 订阅和 API 中转使用同一命令，通过完整名称选择：

```sh
cxa remove personal
```

确认后只删除该账号在 cxa 中保存的凭据和额度缓存，不删除 Codex 会话、不修改当前登录或配置，也不撤销服务端 token 或 API key。删除后无法从 cxa 恢复，需要重新登录、导入凭据或重新输入 key。其他账号的名称保持不变。

当前选中的账号，以及仍被 Codex 凭据文件引用的账号，不能移除；先切换到另一个 ChatGPT 账号。脚本调用必须明确跳过确认：

```sh
cxa remove personal --yes
```

## 添加 API key

在 `cxa add` 向导中选择 API 中转，或直接指定类型：

```sh
cxa add --type api
```

依次输入 API base URL、API key、可选的中转站网站地址，再选择服务类型和账号名称。服务类型可以输入 `generic` 或 `cpa`，直接回车默认选择 `generic`；账号名称默认使用 API 地址的主机名。key 在终端中可见，不要分享包含 key 的截图或终端录屏。网站地址会显示在 `cxa status` 和 `cxa list` 中，方便打开网站查看额度；`cxa` 不自动访问该地址。

新账号默认使用 `generic` 类型，不查询额度，也不探测 CPA 额度接口。需要 CPA 额度查询时，明确指定类型：

```sh
cxa add --type api --api-type cpa
```

类型通过 `--api-type generic` 或 `--api-type cpa` 选择，不根据 URL 名称、HTTP 错误或额度接口缺失猜测。旧版本已保存的 CPA 账号继续按 CPA 处理，不需要重新添加。

脚本添加 API 账号必须提供 `--base-url` 和 `--api-key-stdin`，可选提供 `--name` 和 `--website`：

```sh
cxa add --type api --name gateway --base-url https://gateway.example.com/v1 --website https://gateway.example.com/dashboard --api-key-stdin < /secure/path/gateway-key.txt
```

### CPA 添加示例

如果当前已有 ChatGPT 登录，先执行 `cxa init` 保存该登录，再切换到 CPA。`company` 是自定义名称，以下 URL 是示例，需要替换为实际的 CPA API 地址：

```sh
cxa add --type api --api-type cpa --name company --base-url https://cpa.example.com/v1
```

随后输入 key，再输入可选的网站地址。脚本可以增加 `--api-key-stdin` 从标准输入读取，避免把 key 放在命令参数和 shell 历史中。例如，从私有文件读取：

```sh
cxa add --type api --api-type cpa --name company --base-url https://cpa.example.com/v1 --api-key-stdin < /secure/path/cpa-key.txt
```

文件路径是示例。限制该文件的访问权限，不要提交到 Git。优先使用 HTTPS；即使地址位于内网，HTTP 仍会明文传输 key 和请求。

添加操作只校验输入格式，将 key 以 `0600` 权限保存到 `~/.codex-auth/profile-N/api.json`。添加操作不验证网关认证、不切换账号，也不修改当前登录或 `config.toml`。

先查询服务公布的模型 ID：

```sh
cxa models company
```

再切换连接：

```sh
cxa use company
```

切回个人账号时，仍使用相同的命令：

```sh
cxa use personal
```

每次切换后，都需要让正在运行的 Codex 进程重新加载连接和认证。无需使用 `codex --profile`，也无需让 Desktop 从终端继承 API key 环境变量。

### 模型检查的边界

切换到任何 API key 账号前，`cxa use` 查询 `/models`。如果用户级 `config.toml` 显式设置的 `model` 不在返回列表中，或者模型查询失败，切换会停止，保留当前登录和配置。`cxa` 不自动替换模型。

Desktop、配置 profile、项目配置和已有会话可以选用不同于全局默认值的模型。需要在当前聊天中也选择网关支持的模型；仅重启进程不会让一个不支持的模型变得可用。未显式配置默认模型时，`cxa` 无法提前检查 Codex 最终选择的模型。

`cxa models` 不发起推理请求。模型 ID 出现在列表中，或者额度查询成功，都不代表推理、工具调用和其他 Responses 功能一定兼容。`cxa` 不修改聊天历史，也不替换 Codex 的模型选择器。

## 额度查询

`generic` 账号不会发起额度请求。`status`、`list` 和 `watch` 显示「Quota query disabled (generic API).」，并展示保存的网站地址。没有自动额度查询不代表无限额度；需要到中转站网站查看。

ChatGPT 账号通过隔离的 `codex app-server` 调用 `account/rateLimits/read`。Codex 返回已用比例，`cxa` 转换为剩余比例，保留窗口长度和重置时间。即使当前使用 CPA，个人账号的额度查询也使用明确的官方 ChatGPT 连接，不走 CPA 网关，也不恢复旧连接快照。

CPA 查询的是分配给该 key 的额度，不是池中某个 Pro 账号的原始订阅额度。请求方法与路径为：

```http
GET <base URL 去掉末尾 /v1>/v0/resource/plugins/cpa-key-billing/subscription
Authorization: Bearer <CPA API key>
```

这是服务端 HTTP 接口约定，不要求在客户端安装插件。`cxa` 不安装或加载任何服务端插件。不是每个 CPA 部署都提供该额度接口；服务可以支持推理但不支持此处的额度查询。

`cxa` 读取 `subscription.windows[].dimensions[]` 中的所有额度维度，显示剩余比例和金额、token 数或请求数。窗口名来自 `name`，周期来自 `period_seconds`，重置倒计时来自 RFC 3339 格式的 `end_at`。完整重置时间保存在额度缓存中。没有重置时间时显示未提供，不推测一个时间。

例如，标题「开发团队 · USD」表示服务端命名为「开发团队」的额度窗口，按美元金额计量（`metric = "amount_usd"`），不代表模型名称或 ChatGPT 套餐。`$73.24 / $260.00 left` 表示该窗口分配的 260 美元额度中剩余 73.24 美元，不表示个人充值余额或池中订阅的原始额度。

两类账号使用相同的窗口显示布局；CPA 额外显示分配额度的数值：

```text
  1. personal · Plus
     personal@example.com · updated just now
     Codex
       5-hour   [████████████████]  100% left  resets in 4h 58m
       Weekly   [████████████████]  100% left  resets in 6d 17h

* 2. company · CPA API key
     https://cpa.example.com/v1 · updated just now
     Core · USD
       Weekly   [██████████████░░]   90% left  resets in 5d 7h
                $360.00 / $400.00 left
```

最左侧的 `*` 和绿色加粗标题表示 Codex 凭据和连接文件中当前选中的账号；关闭颜色时仍保留 `*`。运行中的 Codex 进程仍需在切换后重启。每块标题显示展示序号、唯一名称、套餐或账号类型，下一行显示邮箱或 API 地址及额度更新时间。

标记和右对齐的序号占据独立的左侧区域。邮箱、地址和额度分组与名称列对齐，具体额度再向内缩进两个空格。金额行对齐进度条起点，周期标签单独占一列。所有条目采用相同列宽，超过 9 项时也保持对齐。HTTP 错误、连接错误和响应格式错误会显示为查询失败，不会被当成无限额度。刷新失败时，之前成功取得的数据会明确标为缓存并显示刷新错误。

查询不发送推理请求。结果默认缓存 120 秒；交互终端会并行加载各账号数据，重定向输出则在加载完成后一次性输出。ChatGPT token 刷新由 Codex 负责，`cxa` 校验账号身份后回存到同一个账号，不用其他账号的凭据覆盖它。

持续查看额度：

```sh
cxa watch
```

默认每 60 秒刷新，可使用 `--interval SECONDS` 修改，按 `q` 或 Ctrl-C 退出。`cxa list --watch` 仍可使用。

## 切换原理与配置修改范围

账号保存在 `~/.codex-auth/profile-N/` 中，ChatGPT 账号保存 `auth.json`，API key 账号保存 `api.json`。后者还保存 `api_type` 和可选的 `website`；旧文件没有 `api_type` 时按 CPA 读取。不会为每个账号创建独立的日常 Codex home，也不改变当前 `model_provider`，因此不会因本工具切换 provider ID 而将默认会话列表分到不同的 provider 下。能否继续已有聊天，还取决于模型和网关协议是否支持。

| 切换方式 | `config.toml` | `$CODEX_HOME/auth.json` |
| --- | --- | --- |
| 选择 ChatGPT 订阅（包括订阅之间切换） | 明确设置官方连接，保留原 provider ID | 原子替换为选中账号的 OAuth 凭据 |
| 自定义 provider 切到 API key | 仅修改当前 provider 的连接字段 | 保留当前 OAuth 文件 |
| 内建 `openai` provider 切到 API key | 仅修改 `openai_base_url` | 写入 API key 认证 |
| 从 API key 切回 ChatGPT | 明确设置官方连接，不恢复旧中转快照 | 选择保存的 OAuth 凭据 |

自定义 provider 的连接字段限定为 `name`、`base_url`、`wire_api`、`requires_openai_auth`、`experimental_bearer_token`、`env_key` 和 `auth`。例如，已有 `model_provider = "unicodex"` 时，仍保留 `unicodex`，不创建新的 provider。API 模式使用账号名称作为 `name`，设置 `requires_openai_auth = false`，将 key 写入 `experimental_bearer_token`，并移除 `env_key` 和 `auth`，使 Desktop 也能直接读取。

选择 ChatGPT 订阅时，设置 `name = "OpenAI"`（区分大小写）、`wire_api = "responses"`、`requires_openai_auth = true`，移除当前 provider 的 `base_url`、`env_key`、`experimental_bearer_token` 和 `auth`，同时移除顶层 `openai_base_url`。因此，即使最初配置指向某个中转，也不会将订阅请求发回那个中转。内建 `openai` 模式只移除顶层地址覆盖并安装目标 OAuth 凭据，不创建 provider 表。隔离的订阅登录和额度查询也使用同一官方连接规则，不修改真实配置。

模型、推理强度、MCP、项目设置、与连接无关的 provider 参数及 TOML 注释保留在原配置中。在 API 模式下修改这些通用设置，切回 ChatGPT 时也会保留修改。

多文件切换使用私有恢复日志，失败或中断后恢复之前的文件。API 模式下如果手动修改了本工具管理的连接字段，后续切换会报告冲突，不覆盖那些修改。需要修改连接字段时，先使用 `cxa` 切回 ChatGPT；通用配置可以在任何时候修改。

账号身份包括 ChatGPT 用户 ID，以及可用时的 workspace ID。相同邮箱下的不同 workspace 不会合并成同一个账号。API 账号不使用 OAuth 登录，不支持 `cxa relogin`。

### 凭据安全

凭据和连接恢复文件以 `0600` 权限写入，但文件权限不是加密。API key 除了保存在 `api.json`，在 API 模式下还会复制到当前自定义 provider 的 `config.toml`，或内建 `openai` 的 `auth.json`。不要将这些文件、连接快照、恢复日志或整个账号目录提交到仓库、贴到 Issue 或分享给其他人。

模型与额度请求不会跟随 HTTP 重定向，避免将 Bearer key 转发给另一个地址。

## 认证存储

`cxa` 要求文件型认证存储。Codex 默认使用 `file`；各模式的含义见[官方认证文档](https://developers.openai.com/codex/auth#credential-storage)：

- `file`：凭据写入 `$CODEX_HOME/auth.json`。
- `keyring`：凭据写入操作系统凭据存储，不可用时失败。
- `auto`：优先使用操作系统凭据存储，不可用时回退到文件。
- `ephemeral`：仅保留在当前进程内存中。

如果之前显式选择了非 `file` 模式，需要先将 `~/.codex/config.toml` 中的配置改为：

```toml
cli_auth_credentials_store = "file"
```

更改存储配置不会自动迁移操作系统凭据。只有没有有效的文件型登录时，才需要执行 `codex login`，再运行 `cxa init`。如果已经有有效的文件型登录，直接导入，不需要重新登录。

只有某个账号的 refresh token 已失效时，才需要为该账号重新认证：

```sh
cxa relogin <account>
```

## 命令与环境变量

| 命令 | 用途 |
| --- | --- |
| `cxa` / `cxa status` | 查看当前账号、额度和凭据状态 |
| `cxa init` | 保存当前 ChatGPT 登录 |
| `cxa add` | 统一添加向导，选择 ChatGPT 订阅或 API 中转 |
| `cxa add --type chatgpt` | 添加 ChatGPT 订阅，交互选择浏览器或设备码登录 |
| `cxa add --type chatgpt --name NAME` | 添加 ChatGPT 订阅并指定唯一名称 |
| `cxa add --type chatgpt --device-auth` | 直接使用设备码添加 ChatGPT 订阅 |
| `cxa add --type api` | 交互式添加普通 API key，可选保存网站地址 |
| `cxa add --type api --api-type cpa` | 交互式添加 CPA key，启用 CPA 额度查询 |
| `cxa models [account]` | 查询指定 API key 账号的模型，省略账号时使用当前账号 |
| `cxa list` | 查询账号列表和剩余额度 |
| `cxa watch` / `cxa list --watch` | 持续显示额度 |
| `cxa use <account>` | 按完整名称切换，不区分大小写 |
| `cxa rename <account> <name>` | 修改唯一名称 |
| `cxa remove <account>` | 确认后移除保存的账号 |
| `cxa relogin <account>` | 为保存的 ChatGPT 账号重新认证 |
| `cxa import <auth.json>` | 导入已有 OAuth 凭据文件 |

| 环境变量 | 用途 |
| --- | --- |
| `CODEX_HOME` | Codex home，默认 `~/.codex` |
| `CXA_ACCOUNT_STORE` | 账号目录，默认 `~/.codex-auth` |
| `CXA_CODEX_BIN` | 登录和 ChatGPT 额度查询使用的 Codex 可执行文件 |
| `CXA_USAGE_TTL` | 额度缓存有效期，默认 120 秒 |
| `CXA_SKIP_USAGE_REFRESH=1` | 仅显示缓存，不刷新额度 |

路径变量必须使用绝对路径。非交互导入当前登录可使用 `cxa init --yes`。完整选项见 `cxa --help` 和各子命令的 `--help`。

## 常见问题

### 切换后仍显示旧账号

先用 `cxa list` 确认选中账号，再重启实际承载聊天的进程。新开一个聊天窗口不等于重启后台，运行中的 App Server 会保留内存里的认证。`cxa` 只切换文件，不替运行中的进程重新加载认证。

### 模型报 unknown 或找不到

执行 `cxa models company`，确认全局默认模型和当前聊天使用的模型都在服务公布的列表中。模型预检查只检查显式的用户级默认值，不检查所有项目配置、profile 或已存在聊天的模型。

### CPA 额度查询失败

确认服务端为该 key 提供上述订阅接口。接口缺失不代表推理 key 一定无效，查询失败也不代表无限额度。不要把个人 ChatGPT 的原始订阅额度与 CPA 分配额度混为一谈。

### CLI 提示后台功能设置不兼容

Codex `0.161.0` 在连接共享 App Server 前，会核对共享功能开关。例如，CLI 要求启用 `api_key_model_discovery`，后台却关闭时，CLI 会拒绝连接。这与账号过期或 key 失效不同，`cxa` 不管理这些功能开关。

按报错指出的开关统一客户端和后台设置。Codex `0.161.0` 还支持仅本次不用共享后台的显式启动方式：

```sh
codex --no-daemon
```

该命令不会修复共享后台。如果后台不由 Codex daemon 管理，应通过实际管理它的启动器修改或重启。不要为解决功能开关冲突而删除凭据或退出登录。

## 开发与验证

完整检查脚本还需要 Ruby 及其标准 YAML 库；如果安装了 `actionlint`，脚本也会执行工作流检查。

运行仓库检查：

```sh
./scripts/check.sh
```

编译发布二进制：

```sh
cargo build --locked --release --bin cxa
```

对已安装的 Codex 运行离线连接格式测试：

```sh
CXA_REAL_CODEX_BIN="$(command -v codex)" cargo test --locked --test codex_compat -- --ignored
```

此测试仅使用临时 home、占位 key 和本地回环地址，不测试当前登录，也不发起推理请求。仓库包含 Linux 和 macOS 的 CI；发布归档包含两种语言的 README。Homebrew 发布任务仅在上游仓库运行，本 fork 未提供独立的 Homebrew 分发。推送 `main` 不会自动创建二进制发布。

## 上游与许可证

原始 OAuth 账号切换和发布工具来自 [jesse-merhi/cxa](https://github.com/jesse-merhi/cxa)。本 fork 保留命令名称和 OAuth 账号文件格式，增加 CPA 支持。许可证为 [MIT](LICENSE)。
