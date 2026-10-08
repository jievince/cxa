# cxa

[![CI](https://github.com/jievince/cxa/actions/workflows/ci.yml/badge.svg)](https://github.com/jievince/cxa/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/jievince/cxa)](LICENSE)

English | [简体中文](README.zh-CN.md)

`cxa` switches between multiple ChatGPT subscription accounts and CLIProxyAPI
(CPA) API-key accounts in Codex, and shows their remaining quota. These are the
two supported account types; arbitrary API-key services are not supported.
CPA quota queries use the `cpa-key-billing` subscription endpoint.

This is a fork of [jesse-merhi/cxa](https://github.com/jesse-merhi/cxa), with CPA
account switching, model checks, and a unified remaining-quota display. Install
from **this fork** to use these features; upstream binaries and the upstream
Homebrew tap do not contain this fork's CPA implementation.

Use the same Codex home and `model_provider` with Codex CLI and Desktop. No
launcher wrapper or `--profile` is needed. ChatGPT-to-ChatGPT switches change
credentials only; CPA switches also update the connection settings described
in [How it works](#how-it-works), without replacing the rest of `config.toml`.

## Demo

Install `cxa`, import the current Codex login, load every account's quota in
parallel, and switch accounts:

https://github.com/user-attachments/assets/25842439-7304-480a-a0b0-21b9e4c7d18b

## Requirements

- The [Codex CLI](https://developers.openai.com/codex/cli) installed and
  available as `codex`
- A ChatGPT OAuth login and/or a CLIProxyAPI (CPA) API key
- For CPA: an OpenAI-compatible Responses API and `/models`; quota queries
  additionally require the service's subscription endpoint documented below
- CPA switching supports built-in `openai` or an already configured custom
  provider; `ollama`, `lmstudio`, and `amazon-bedrock` are not converted
- macOS or Linux
- A current stable Rust toolchain with `cargo` to install this fork from source
- A native C compiler and linker (for example, Linux `build-essential` or macOS
  Command Line Tools)

The offline connection-format test has passed with Codex CLI `0.161.0` on
Linux, using isolated homes and dummy keys. It checks that Codex accepts both
supported provider configurations; it does not prove every gateway or Codex
Desktop version supports all Responses features. Codex upgrades can change
authentication, configuration, and app-server APIs. Compatibility is tested,
not guaranteed across future versions.

## Install

Install the current `main` branch of this fork:

```sh
cargo install --locked --git https://github.com/jievince/cxa --branch main --bin cxa
```

Cargo normally installs the executable in `~/.cargo/bin`. Keep that directory
on `PATH`. If another `cxa` is installed through Homebrew or in `~/.local/bin`,
check which executable the shell selects:

```sh
command -v cxa
```

Update an existing Cargo installation from this fork:

```sh
cargo install --locked --force --git https://github.com/jievince/cxa --branch main --bin cxa
```

Alternatively, install from a checkout of this fork:

```sh
cargo install --locked --path . --bin cxa
```

These commands install the executable; they do not sign out, select an account,
or replace existing profiles. This fork's CPA build is distributed from source;
do not use an upstream release archive to install the features documented here.
Windows is not currently supported.

## Quick start

Make sure Codex is signed in with ChatGPT OAuth, then import that account:

```sh
cxa init
```

If Codex is not signed in yet, run `codex login` first. Codex uses the file
credential store by default; the [credential storage](#credential-storage)
section covers custom configurations.

Add another account. `cxa` will open the normal Codex login flow:

```sh
cxa add
```

List your accounts and their latest known quota:

```sh
cxa list
```

Switch accounts by number or by a unique part of the email address:

```sh
cxa 2
```

```sh
cxa use work@example.com
```

Restart any running Codex or ChatGPT session after switching so it loads the
new account. `cxa` does not restart those processes automatically. For Desktop,
this includes the app-server on a remote development machine when one is used.

### Add a CLIProxyAPI (CPA) API-key account

If Codex is already signed in with ChatGPT, enroll that login with `cxa init`
before switching, so its credentials are preserved. Then add a CPA account:

```sh
cxa add --api-key --name company --base-url https://cpa.example.com/v1
```

Enter the CPA key at the hidden prompt. `company` is an arbitrary display name
and the URL above is an example: use your CPA service's actual API URL.
For scripts, add `--api-key-stdin` and supply the key on stdin; do not put the
secret in command arguments.

For example, read a private key file into stdin:

```sh
cxa add --api-key --name company --base-url https://cpa.example.com/v1 --api-key-stdin < /secure/path/cpa-key.txt
```

The path is an example. Keep the file private and do not commit it.

This workflow supports CPA keys only, not arbitrary third-party API keys. CPA
must expose the OpenAI Responses API and `/models`. To show quota, the service
must also expose `GET /v0/resource/plugins/cpa-key-billing/subscription` with
Bearer authentication. This is a server-side API contract, not a client plugin
dependency. `cxa` does not install or load server plugins; it only requests
this endpoint and parses the returned subscription. Not every CPA deployment
provides this billing endpoint.

Prefer HTTPS. An HTTP URL sends the key and requests without transport
encryption, even when the service is on a private network.

Adding an account only validates the local input and saves the key with mode
`0600` in `~/.codex-auth/profile-N/api.json`. It does not test authentication,
switch accounts, or modify the current login or `config.toml`. `cxa use` checks
advertised models and switches the connection; `cxa list` queries quota. Neither
the model-list check nor a quota read proves inference compatibility.

The daily commands stay the same:

```sh
cxa list
```

```sh
cxa use company
```

```sh
cxa use personal@example.com
```

Restart the running Codex CLI or Desktop app-server after switching. Launch
Codex normally: no `--profile` option or environment variable is needed.

`cxa` preserves the current `model_provider` ID, including a custom ID such as
`unicodex`, and keeps the same Codex home and session files. This avoids
splitting the default session list by provider. Continuing a session also
depends on the gateway supporting the selected model and Responses features.

Before an API switch, `cxa use` queries the gateway's `/models` endpoint. If
the explicit user-level `model` in `config.toml` is not advertised, the switch
fails without changing the current login or configuration. A failed model-list
request also stops the switch. No model is substituted automatically. A
successful switch prints the advertised IDs, but this is not an inference or
feature-compatibility check.

Desktop and resumed chats can select models independently of the user-level
default. Select a gateway-supported model in the chat too: restarting alone
does not make an unsupported model work. `cxa` does not rewrite chat history or
filter Codex's model picker. When no explicit default is configured, the model
chosen by Codex cannot be checked in advance; use one of the printed IDs.

Query an API account's advertised models without switching:

```sh
cxa models company
```

Omit the account to query the selected API account. This command does not send
an inference request or consume model tokens.

CPA quota is the allocation for the enrolled API key, not the Pro subscription
quota of an underlying pool account. The query is an authenticated HTTP GET:

```http
GET <CPA base URL without trailing /v1>/v0/resource/plugins/cpa-key-billing/subscription
Authorization: Bearer <CPA API key>
```

All returned windows and dimensions are shown with remaining percentages and
remaining amounts. Window names and periods come from `name` and `period_seconds`;
`end_at` supplies the reset countdown; the complete reset timestamp is retained
in the quota cache. ChatGPT and CPA windows use the same compact row format:
period, progress bar, remaining percentage, and reset countdown. CPA allocation
amounts appear below that row. If no reset time is returned, the display says so
instead of inferring one.
Unlimited allocation, missing data, and HTTP failures are
displayed separately. An unavailable billing endpoint is a quota-query failure, not
an unlimited allocation. If a refresh fails, cached successful data is
explicitly marked with the refresh error.

For ChatGPT accounts, `cxa` starts an isolated Codex app-server for each stored
login and calls `account/rateLimits/read`. It converts `usedPercent` to remaining
percentages and uses the returned window durations and reset timestamps.
Refreshed OAuth credentials are saved back to the same account profile; they
are not replaced with another account's credentials. No inference request is
sent for either kind of quota query. Results are cached for 120 seconds by
default.

## Example

```text
$ cxa list
* 1  personal@example.com  Pro 20x · updated just now
    Codex
      Weekly   [█████████████░░░]   82% left  resets in 6d 11h
    Codex Spark
      5-hour   [█████████░░░░░░░]   59% left  resets in 4h 12m
      Weekly   [███████████████░]   91% left  resets in 6d 23h

  2  work@example.com  Pro 20x · updated just now
    Codex
      Weekly   [██████░░░░░░░░░░]   37% left  resets in 3d 8h
    Codex Spark
      5-hour   [████████████████]  100% left  resets in 4h 48m
      Weekly   [██████████████░░]   88% left  resets in 5d 17h

  3  company  CPA API key · updated just now
    Core · USD
      Weekly   [██████████████░░]   90% left  resets in 5d 7h
               $360.00 / $400.00 left

$ cxa 2
✓ Account 2 (work@example.com) is now selected.
! Restart Codex or ChatGPT before expecting an existing session to use this account.
```

The `*` marks the account currently selected in Codex.

When quota data is stale, an interactive terminal immediately lists every
account with an animated loading indicator, fetches them in parallel, and fills
each account in as it responds. Redirected output skips the live display and
prints the completed list once.

Keep the dashboard open with `cxa watch`. It refreshes every 60 seconds; use
`--interval SECONDS` to change the cadence, and press `q` or Ctrl-C to exit.
`cxa list --watch` remains available as the explicit form.

## Commands

| Command | Description |
| --- | --- |
| `cxa` | Show the selected account and credential state |
| `cxa init` | Import the current Codex login as account 1 |
| `cxa add` | Sign in and add another account |
| `cxa add --device-auth` | Add an account with Codex's device-code flow |
| `cxa add --api-key --name NAME --base-url URL` | Add a CPA API key using a hidden prompt |
| `cxa add --api-key --name NAME --base-url URL --api-key-stdin` | Add a CPA API key supplied on stdin |
| `cxa models <account>` | Query a CPA account's advertised models without switching |
| `cxa list` | List accounts and their latest known quota |
| `cxa watch` | Keep the live quota dashboard open |
| `cxa watch --interval 30` | Refresh every 30 seconds |
| `cxa list --watch` | Open watch mode through `list` |
| `cxa <account>` | Switch by account number, email, or API account name |
| `cxa use <account>` | Switch using the explicit command form |
| `cxa status` | Show the selected account and credential state |
| `cxa relogin <account>` | Re-authenticate a saved account |
| `cxa import <auth.json>` | Import an existing Codex credential file |

Use `cxa --help` or `cxa <command> --help` for the complete CLI reference.

## How it works

Each account is stored as a profile under `~/.codex-auth`. Existing OAuth
profiles keep their original `auth.json` format. API profiles use `api.json`.
Secrets and connection recovery files are written with mode `0600`.

When a ChatGPT connection is already active, switching between ChatGPT accounts
atomically replaces `$CODEX_HOME/auth.json`; it does not edit the config.

For an API account using a custom provider, `cxa` keeps the provider ID and
edits only that provider's `base_url`, `wire_api`, `requires_openai_auth`,
`experimental_bearer_token`, `env_key`, and `auth` settings. The gateway key
is stored in `experimental_bearer_token`, so Desktop can load it without a
shell environment. The live OAuth credential file is left in place. Returning
to ChatGPT restores these settings and selects the saved OAuth credentials.
Models, reasoning settings, MCP servers, projects, unrelated provider tuning,
and TOML comments remain in the live config; edits to those settings while
using the API account are retained.

For the built-in `openai` provider, `cxa` changes only `openai_base_url` in the
config and installs API-key credentials in `auth.json`. The current OAuth
login must be enrolled first, and its latest credentials are saved before the
switch. Returning to ChatGPT restores the original URL and saved credentials.
No custom provider is created.

Multi-file switches have a private recovery journal. Failed or interrupted
switches restore the previous files. If connection fields were manually
changed while an API account was active, `cxa` reports the conflict instead of
overwriting those edits. Switch back with `cxa` before editing connection
fields. Other config edits can be made at any time.

Codex keeps credentials in memory while it is running. Switching is safe, but
an existing Codex or ChatGPT process will continue using its previous account
until you restart it.

To read quota, `cxa` runs `codex app-server` with the saved account in an
isolated temporary home and the saved OAuth route, even when the company
account is selected. This does not change the selected account. Codex owns
OAuth token refresh; if it refreshes a token during a quota read, `cxa` verifies
the account identity before saving the updated credentials.

Account identity includes the ChatGPT user ID and, when available, the
workspace ID. Accounts and workspaces that share an email address remain
distinct.

For API accounts, `cxa` uses the saved key to query
`/v0/resource/plugins/cpa-key-billing/subscription`. It removes a trailing
`/v1` from the base URL while retaining any reverse-proxy path prefix. It does
not follow HTTP redirects. Supported API-key accounts are limited to CPA;
other API-key services have no supported quota or switching adapter.
API accounts do not use OAuth `cxa relogin`.

The key remains in its `api.json` profile. While a custom-provider API account
is active, a copy is also present in the provider's `experimental_bearer_token`
in `config.toml`. For built-in `openai`, it is copied into `auth.json` instead.
Do not publish those files, connection snapshots, recovery journals, or your
account-store directory. File permissions are not encryption.

## Credential storage

`cxa` supports Codex's default file-backed credential store, documented in
[Codex authentication](https://developers.openai.com/codex/auth#credential-storage).
If the current file-backed login works, import it with `cxa init`; do not log
out or log in again just to install or update `cxa`. If you configured
`cli_auth_credentials_store` as `keyring`, `auto`, or `ephemeral`, change it to
`file` before using `cxa`:

```toml
# ~/.codex/config.toml
cli_auth_credentials_store = "file"
```

Changing the storage setting does not migrate OS-stored credentials. If no
valid file-backed login exists, run `codex login`, then `cxa init`. If a valid
file-backed login already exists, import it without signing in again.

If an account's refresh token is no longer valid, re-authenticate it:

```sh
cxa relogin <account>
```

## Troubleshooting

### The switched account still looks unchanged

Confirm the selected profile with `cxa list`, then restart the Codex process
that is actually serving the chat. Opening another chat inside an existing
Desktop app-server does not itself reload credentials. `cxa` changes the files,
not the in-memory authentication of an already running server. Restarting an
app-server can interrupt active work; finish that work before restarting.

### A model is unknown or missing

Run `cxa models company` and use an advertised model in both the global default
and the current chat. `cxa use` checks only an explicit user-level default;
Desktop, profiles, project configuration, and resumed chats can select a
different model. `cxa` does not silently substitute a model or rewrite history.
An advertised ID is not a guarantee of inference or tool-feature compatibility.

### CPA quota is unavailable

The gateway can support inference without exposing the billing endpoint.
`cxa list` reports HTTP, connection, and response-format failures rather than
showing an unlimited allocation. Confirm that the service administrator exposes
the endpoint above for the enrolled key. A quota-query failure does not prove
the inference key is invalid. Missing reset metadata is displayed as unknown;
`cxa` does not invent a reset time.

### Background server has incompatible feature settings

With Codex `0.161.0`, the CLI checks shared app-server feature settings before
connecting. This error is distinct from a rejected key or an expired login.
Check the required feature named by the CLI and align it with the running
server. For example, a server with `api_key_model_discovery = false` conflicts
with a CLI requiring it to be enabled. `cxa` does not manage those feature flags.

Codex `0.161.0` also provides an explicit, one-time independent launch:

```sh
codex --no-daemon
```

This does not fix the shared server. If Codex says the server is not managed by
its daemon, restart or reconfigure it through the launcher that owns it. Do not
delete credentials or log out to fix a feature-setting mismatch.

## Configuration

| Variable | Purpose |
| --- | --- |
| `CODEX_HOME` | Override the Codex home directory |
| `CXA_ACCOUNT_STORE` | Override the account profile directory |
| `CXA_CODEX_BIN` | Override the Codex executable used for login and quota reads |
| `CXA_USAGE_TTL` | Set the quota cache lifetime in seconds (default: `120`) |
| `CXA_SKIP_USAGE_REFRESH=1` | Show cached quota without refreshing it |

Values supplied to path variables must be absolute.

For non-interactive setup, use `cxa init --yes`.

## Development

The full check script also requires Ruby with its standard YAML library.
If `actionlint` is available, the script runs it as well.

```sh
./scripts/check.sh
```

```sh
cargo build --locked --release --bin cxa
```

Run the offline compatibility test against an installed Codex binary:

```sh
CXA_REAL_CODEX_BIN="$(command -v codex)" cargo test --locked --test codex_compat -- --ignored
```

This test uses temporary homes, dummy keys, and a loopback URL. It does not
use the current login or send an inference request.

CI checks formatting, Clippy, tests, release packaging, and Linux and macOS
builds. Release archives include both README languages. Homebrew publishing
jobs run only in the upstream repository; this fork does not provide its own
Homebrew distribution. Pushing `main` does not create a binary release.

## Upstream

The original OAuth account switcher and release tooling are from
[jesse-merhi/cxa](https://github.com/jesse-merhi/cxa). This fork keeps the same
command names and OAuth profile format while adding CPA support.

## License

[MIT](LICENSE)
