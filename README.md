# cxa

[![CI](https://github.com/jievince/cxa/actions/workflows/ci.yml/badge.svg)](https://github.com/jievince/cxa/actions/workflows/ci.yml)

English | [简体中文](README.zh-CN.md)

Switch between ChatGPT subscriptions and Responses-compatible API gateways in Codex CLI and Desktop. Keep one Codex home and the existing `model_provider` ID; no launcher or `--profile` required.

- ChatGPT subscriptions: isolated login, account switching, remaining quota.
- API gateways: key switching, model listing, optional service website.
- CLIProxyAPI (CPA): subscription quota and reset times when exposed by the service.

This fork extends [jesse-merhi/cxa](https://github.com/jesse-merhi/cxa). Install from this repository; upstream releases and the upstream Homebrew tap do not include these API-gateway features.

## Install

Requires macOS or Linux, Codex CLI on `PATH`, stable Rust, and a native C compiler/linker.

```sh
cargo install --locked --git https://github.com/jievince/cxa --branch main --bin cxa
```

Update:

```sh
cargo install --locked --force --git https://github.com/jievince/cxa --branch main --bin cxa
```

Cargo installs into `~/.cargo/bin` by default. Ensure it is on `PATH`; use `command -v cxa` to check for an older installation elsewhere. Installation does not change accounts or Codex configuration.

## Start

Save your current ChatGPT login without signing out:

```sh
cxa init
```

Add a ChatGPT subscription or API connection:

```sh
cxa add
```

ChatGPT login runs in an isolated home. The API wizard asks for the base URL, API key, optional website, service type, and optional name. Choose `generic` for a gateway without quota queries, or `cpa` for a CPA service with its subscription endpoint. Adding an account saves it without switching.

List accounts and quota:

```sh
cxa list
```

Switch by name, then restart the Codex CLI or Desktop processes that should use it:

```sh
cxa use company
```

Both account types accept `--name`. Otherwise, the default is the email local part or API hostname. Generated names gain a suffix on collision; explicit names must be unique, ignoring case. Names contain 1–64 ASCII letters, digits, `.`, `_`, or `-`, start with a letter or digit, and cannot be entirely numeric.

## Everyday commands

| Command | Purpose |
| --- | --- |
| `cxa status` | Selected account and credential state; also the default for `cxa` |
| `cxa list` | All accounts; `*` marks the selected one |
| `cxa watch` | Live quota view; `q` or Ctrl-C exits |
| `cxa use NAME` | Switch using an exact account name |
| `cxa rename OLD NEW` | Rename without changing live credentials or config |
| `cxa remove NAME` | Remove a saved account; active accounts are protected |
| `cxa models [NAME]` | Query an API gateway's `/models` |
| `cxa relogin NAME` | Renew a ChatGPT login |
| `cxa import PATH` | Import a ChatGPT `auth.json` |

List numbers are display positions, starting at 1; commands accept names only. There is no `cxa NAME` shorthand. Run `cxa COMMAND --help` for options.

## Configuration boundary

cxa never changes the top-level `model_provider` ID or edits session rollouts and databases. It does update the selected provider's connection settings: ChatGPT selects the official OAuth route; an API connection selects its URL and key. Models, reasoning settings, MCP servers, projects, and unrelated settings remain in place.

With an existing custom provider such as `unicodex`, its `name` becomes `OpenAI` for ChatGPT or the current cxa account name for an API connection. Renaming takes effect in the provider on the next `cxa use`. Without a custom provider, cxa keeps built-in `openai` and uses `openai_base_url` for gateways.

Credentials require Codex's `file` store. Secrets are stored locally with mode `0600`, without encryption. Existing Codex processes need a restart after switching. Gateway support requires both the Responses API and `/models`; cxa checks advertised models before switching but cannot guarantee every model works in an existing chat.

See the [reference](docs/reference.md) for exact config/auth changes, scripted enrollment, quota queries, environment variables, and troubleshooting. Compatibility with future Codex versions is not guaranteed.

## Development

```sh
./scripts/check.sh
```

Tests use isolated homes and fake credentials. See [development notes](docs/reference.md#development) for the optional real-Codex format check.

[MIT license](LICENSE).
