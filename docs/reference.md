# cxa reference

[README](../README.md) · [简体中文](reference.zh-CN.md)

## Enrollment and names

`cxa init` imports the current ChatGPT file login; `--yes` skips confirmation. If no login exists, use `cxa add --type chatgpt`. Both enrollment types support an optional `--name`.

For device-code login:

```sh
cxa add --type chatgpt --device-auth --name personal
```

For an API connection, enter the key in the wizard:

```sh
cxa add --type api --name company --api-type cpa
```

For scripts, supply an explicit type, URL, and a key on stdin. Omit `--api-type` for a generic gateway without quota queries:

```sh
cxa add --type api --name gateway --base-url https://gateway.example/v1 --website https://gateway.example/dashboard --api-key-stdin < /secure/path/key.txt
```

Adding saves credentials without validating an API key or changing the active connection. `cxa use NAME` queries `/models` and rejects an unavailable user-level default model before switching. It does not replace the model or inspect every project's settings or an existing chat's model selection.

Names are exact, case-insensitive selectors. ChatGPT defaults to the email local part; APIs default to the hostname. Generated collisions receive `-2`, `-3`, etc.; explicit duplicates fail. Renaming only updates the saved name, with an API provider's display name updated on its next selection. Internal `profile-N` directories are storage details, not selectors.

`cxa remove NAME` asks for confirmation (`--yes` skips it). The selected account and credentials still referenced by Codex are protected. Removal deletes the saved profile and quota cache, not sessions or server-side credentials. API connections do not support `relogin`.

## Provider ID, display name, and credentials

cxa reads the user-level `config.toml`. It never inserts, deletes, or changes `model_provider`. With no explicit value it treats the ID as built-in `openai`. Existing custom IDs require an existing provider table. Built-in `ollama`, `lmstudio`, and `amazon-bedrock` cannot be converted.

| Selection | Existing custom provider, e.g. `unicodex` | Built-in `openai` |
| --- | --- | --- |
| ChatGPT | Set `name = "OpenAI"`, `wire_api = "responses"`, `requires_openai_auth = true`; remove `base_url`, `env_key`, `experimental_bearer_token`, `auth` | Remove top-level `openai_base_url` |
| API | Set `name` to the current cxa name, `base_url`, `wire_api = "responses"`, `requires_openai_auth = false`, `experimental_bearer_token`; remove `env_key`, `auth` | Set top-level `openai_base_url` |
| Live `auth.json` | ChatGPT: install selected OAuth bundle. API: leave the file in place | ChatGPT: install selected OAuth bundle. API: write `OPENAI_API_KEY` |

ChatGPT selection removes top-level `openai_base_url` for either route. It constructs the official connection rather than restoring a previous relay snapshot. `name` is a display label; it does not select built-in OpenAI. The provider ID stays fixed when names or connections change.

For example, selecting an API connection named `company` with an existing `unicodex` provider produces these connection fields:

```toml
model_provider = "unicodex"

[model_providers.unicodex]
name = "company"
base_url = "https://gateway.example/v1"
wire_api = "responses"
requires_openai_auth = false
experimental_bearer_token = "<saved key>"
```

Unrelated fields and their formatting remain in the live document, including edits made while using an API. Comments attached to replaced connection fields may change. cxa does not migrate rollouts or SQLite provider markers, so it does not merge sessions previously created under different IDs.

Saved profiles live in `~/.codex-auth`: OAuth bundles in `profile-N/auth.json`, API details in `profile-N/api.json`, names in `profile-N/name`. With a custom API provider, a key copy is also written into `config.toml`; built-in `openai` uses `auth.json` instead. Connection state and recovery journals may contain secrets. Private files are written with mode `0600`; permissions are not encryption.

Before replacing live OAuth credentials, cxa saves their latest version. For built-in API switching, an existing ChatGPT login must first be imported with `cxa init`. Switches use a recovery journal to restore previous files on failure or recover an interrupted switch on the next run. If an active API connection was edited outside cxa, switching reports a conflict instead of overwriting it. Switch away before editing managed connection fields.

## Quota queries

- **ChatGPT:** run `codex app-server` in an isolated home with the official OAuth route; query `account/read` and `account/rateLimits/read`. Refreshed tokens are saved only after verifying identity. Identity includes the user and workspace IDs, so matching emails do not collapse distinct workspaces.
- **CPA:** use the saved API key as a Bearer token for `GET /v0/resource/plugins/cpa-key-billing/subscription`. Strip a trailing `/v1` from the base URL, retain reverse-proxy path prefixes, and reject redirects. This is a server endpoint; cxa does not install a plugin. The service must expose it.
- **Generic:** do not query quota. An optional website is displayed for manual checks.

CPA group names, limits, periods, and reset times come from the response. `USD` means the assigned dollar budget, not a backend ChatGPT subscription's remaining quota. Missing reset times are not inferred. Query failures show an error and any cached result; they are not shown as unlimited quota. ChatGPT and CPA queries refresh on every `status` or `list` by default. Cached results remain available when a refresh fails.

## Troubleshooting and environment

Codex must use file credentials. If configured otherwise, set `cli_auth_credentials_store = "file"` in `config.toml`. This setting does not migrate credentials from the OS keyring. cxa refuses non-file stores and a nonempty `CODEX_ACCESS_TOKEN` override.

Restart the Codex CLI or Desktop processes after switching: existing processes can retain the old credentials. cxa does not reload or kill them. If a gateway rejects a model, check `cxa models NAME` and the model selected in the affected chat. Model listing does not prove full Responses compatibility.

| Variable | Meaning |
| --- | --- |
| `CODEX_HOME` | Codex directory; default `~/.codex` |
| `CXA_ACCOUNT_STORE` | Profile directory; default `~/.codex-auth`; must not overlap Codex home |
| `CXA_CODEX_BIN` | Absolute path to an alternative Codex executable |
| `CXA_USAGE_TTL` | Quota cache lifetime in seconds; default `0` (refresh every time) |
| `CXA_SKIP_USAGE_REFRESH=1` | Display cached quota without querying |

Directory overrides must be absolute. `cxa watch --interval 30` changes the live refresh cadence; intervals must be at least 5 seconds. It is also available as `cxa list --watch`.

## Development

Run `./scripts/check.sh` for formatting, Clippy, tests, installer, and release checks. Tests isolate `HOME`, `CODEX_HOME`, and `CXA_ACCOUNT_STORE` and use fake credentials; never test switching against a real login.

An optional offline format check starts a real Codex binary with isolated homes and dummy keys:

```sh
CXA_REAL_CODEX_BIN="$(command -v codex)" cargo test --locked --test codex_compat -- --ignored
```

This check has passed with Codex CLI `0.161.0` on Linux. It validates config acceptance, not live gateway behavior or every Desktop version. Authentication, configuration, and app-server contracts can change with Codex upgrades.
