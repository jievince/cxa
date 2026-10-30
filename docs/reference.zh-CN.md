# cxa 参考

[README](../README.zh-CN.md) · [English](reference.md)

## 添加与命名

`cxa init` 导入当前 ChatGPT 文件登录，`--yes` 跳过确认。没有登录时可直接运行 `cxa add --type chatgpt`。两种类型均接受可选的 `--name`。

使用设备码登录：

```sh
cxa add --type chatgpt --device-auth --name personal
```

添加 API 连接，在向导中输入密钥：

```sh
cxa add --type api --name company --api-type cpa
```

脚本添加须明确类型、URL，并从标准输入读取密钥。省略 `--api-type` 即为不查询额度的普通中转：

```sh
cxa add --type api --name gateway --base-url https://gateway.example/v1 --website https://gateway.example/dashboard --api-key-stdin < /secure/path/key.txt
```

添加仅保存凭据，不验证 API key，也不切换连接。`cxa use NAME` 先查询 `/models`；若用户级默认模型不在列表中则拒绝切换。它不自动更换模型，也不检查所有项目配置或已有聊天的模型选择。

名称完整匹配，不区分大小写。ChatGPT 默认取邮箱前缀，API 默认取主机名。生成的名称冲突时追加 `-2`、`-3` 等后缀；显式名称重复则报错。改名只更新保存的名称，API provider 的显示名在下次选择时同步。内部 `profile-N` 目录不是操作标识。

`cxa remove NAME` 需要确认，`--yes` 可跳过确认。当前选中的账号及仍被 Codex 引用的凭据受保护。删除只移除本地账号及额度缓存，不删除会话，也不撤销服务端凭据。API 连接不支持 `relogin`。

## Provider ID、显示名与认证

cxa 读取用户级 `config.toml`，不新增、删除或修改 `model_provider`。未显式设置时按内建 `openai` 处理。已有自定义 ID 必须有对应的 provider 表。内建 `ollama`、`lmstudio`、`amazon-bedrock` 不支持转换。

| 切换目标 | 已有自定义 provider，例如 `unicodex` | 内建 `openai` |
| --- | --- | --- |
| ChatGPT | 设置 `name = "OpenAI"`、`wire_api = "responses"`、`requires_openai_auth = true`；删除 `base_url`、`env_key`、`experimental_bearer_token`、`auth` | 删除顶层 `openai_base_url` |
| API | 设置 `name` 为当前 cxa 名称，以及 `base_url`、`wire_api = "responses"`、`requires_openai_auth = false`、`experimental_bearer_token`；删除 `env_key`、`auth` | 设置顶层 `openai_base_url` |
| 当前 `auth.json` | ChatGPT：写入所选 OAuth 凭据；API：保留原文件 | ChatGPT：写入所选 OAuth 凭据；API：写入 `OPENAI_API_KEY` |

切换 ChatGPT 时，两种路径都会删除顶层 `openai_base_url`，明确使用官方连接，不恢复旧中转快照。`name` 只是显示名，写成 `OpenAI` 不会自动选用内建 provider。名称和连接变化时，provider ID 始终保留。

例如，已有 `unicodex` 时选择名为 `company` 的 API 连接，对应的连接字段如下：

```toml
model_provider = "unicodex"

[model_providers.unicodex]
name = "company"
base_url = "https://gateway.example/v1"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "<saved key>"
```

无关字段及其格式保留，包括使用 API 期间对它们的修改。被替换连接字段附带的注释可能变化。cxa 不迁移 rollout 或 SQLite 的 provider 标记，因此不会合并以前在不同 ID 下创建的会话。

账号默认保存在 `~/.codex-auth`：OAuth 凭据位于 `profile-N/auth.json`，API 信息位于 `profile-N/api.json`，名称位于 `profile-N/name`。自定义 provider 的 API key 还会写入 `config.toml`；内建 `openai` 则写入 `auth.json`。连接状态和恢复日志也可能包含密钥。私有文件写入权限为 `0600`，这不等于加密。

替换当前 OAuth 凭据前，cxa 会先回存其最新版本。使用内建 provider 切到 API 前，已有 ChatGPT 登录须先通过 `cxa init` 保存。多文件切换使用恢复日志，失败时回滚，中断后在下次运行恢复。若当前 API 连接字段被外部修改，cxa 会报告冲突，拒绝覆盖；请先切离该连接，再修改受管理字段。

## 额度查询

- **ChatGPT**：在隔离目录中以官方 OAuth 连接运行 `codex app-server`，调用 `account/read` 与 `account/rateLimits/read`。刷新后的 token 经身份核对才回存。身份包含用户和工作区 ID，同邮箱的不同工作区不会被合并。
- **CPA**：用保存的 API key 作为 Bearer token，请求 `GET /v0/resource/plugins/cpa-key-billing/subscription`。从 base URL 去掉末尾 `/v1`，保留反代路径前缀，不跟随重定向。这是服务端接口，cxa 不安装插件，需服务端提供该接口。
- **普通 API**：不查询额度；可显示用户填写的网站，供手动查询。

CPA 分组名称、额度、周期和重置时间来自响应。`USD` 指分配的美元预算，不是底层 ChatGPT 订阅剩余额度。没有重置时间时不推算。查询失败会标明错误并显示已有缓存，不会按无限额度处理。ChatGPT 和 CPA 默认每次 `status` 或 `list` 都实时刷新；刷新失败时保留缓存供查看。

## 排查与环境变量

Codex 必须使用文件凭据。若配置为其他方式，请在 `config.toml` 设置 `cli_auth_credentials_store = "file"`。该设置不会迁移系统钥匙串中的凭据。cxa 拒绝非文件存储及非空的 `CODEX_ACCESS_TOKEN` 覆盖。

切换后请重启对应的 Codex CLI 或 Desktop 进程，已有进程可能仍持有旧凭据。cxa 不负责重载或终止进程。中转报模型错误时，检查 `cxa models NAME` 和出错聊天所选的模型。模型列表可用不代表完整兼容 Responses API。

| 变量 | 含义 |
| --- | --- |
| `CODEX_HOME` | Codex 目录，默认 `~/.codex` |
| `CXA_ACCOUNT_STORE` | 账号目录，默认 `~/.codex-auth`，不得与 Codex home 重叠 |
| `CXA_CODEX_BIN` | 其他 Codex 可执行文件的绝对路径 |
| `CXA_USAGE_TTL` | 额度缓存秒数，默认 `0`（每次刷新） |
| `CXA_SKIP_USAGE_REFRESH=1` | 只显示缓存，不发起查询 |

目录覆盖值必须是绝对路径。`cxa watch --interval 30` 设置持续刷新的间隔，最小为 5 秒；也可运行 `cxa list --watch`。

## 开发验证

运行 `./scripts/check.sh` 检查格式、Clippy、测试、安装脚本及发布配置。测试隔离 `HOME`、`CODEX_HOME`、`CXA_ACCOUNT_STORE`，使用虚拟凭据；不得对真实登录测试切换。

可选的离线格式检查会用隔离目录和虚拟密钥启动真实 Codex：

```sh
CXA_REAL_CODEX_BIN="$(command -v codex)" cargo test --locked --test codex_compat -- --ignored
```

该检查已在 Linux 的 Codex CLI `0.161.0` 上通过，只验证配置可被接受，不代表真实中转请求或所有 Desktop 版本都兼容。Codex 升级可能改变认证、配置及 app-server 接口。
