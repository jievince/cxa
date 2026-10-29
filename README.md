# cxa

[![CI](https://github.com/jievince/cxa/actions/workflows/ci.yml/badge.svg)](https://github.com/jievince/cxa/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/jievince/cxa)](LICENSE)

English | [简体中文](README.zh-CN.md)

`cxa` switches between multiple ChatGPT subscription accounts and
Responses-compatible API-key gateways in Codex. ChatGPT and CLIProxyAPI (CPA)
accounts support quota queries. Generic API accounts skip quota queries and can
show a service website for checking usage manually.

This is a fork of [jesse-merhi/cxa](https://github.com/jesse-merhi/cxa), with API-key
account switching, model checks, and a unified remaining-quota display. Install
from **this fork** to use these features; upstream binaries and the upstream
Homebrew tap do not contain this fork's API-key implementation.

Use the same Codex home and `model_provider` with Codex CLI and Desktop. No
launcher wrapper or `--profile` is needed. ChatGPT-to-ChatGPT switches change
credentials only; API-key switches also update the connection settings described
in [How it works](#how-it-works), without replacing the rest of `config.toml`.

## Demo

Install `cxa`, import the current Codex login, load every account's quota in
parallel, and switch accounts:

https://github.com/user-attachments/assets/25842439-7304-480a-a0b0-21b9e4c7d18b

## Requirements

- The [Codex CLI](https://developers.openai.com/codex/cli) installed and
  available as `codex`
- A ChatGPT OAuth login and/or a Responses-compatible gateway API key
- For API keys: an OpenAI-compatible Responses API and `/models`; CPA quota
  queries additionally require the service's subscription endpoint documented below
- API-key switching supports built-in `openai` or an already configured custom
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
or replace existing profiles. This fork's API-key build is distributed from source;
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

Add another account with the unified wizard. Choose a ChatGPT subscription or
an API gateway:

```sh
cxa add
```

For ChatGPT, choose browser or device-code login. cxa runs `codex login` in an
isolated home and saves the OAuth credentials without changing your current
login or real `config.toml`. For an API gateway, enter the base URL,
key, optional website, service type, and account name.

Skip the account-type prompt with an explicit type:

```sh
cxa add --type chatgpt
```

On a headless machine, select device-code login directly:

```sh
cxa add --type chatgpt --device-auth
```

Redirected input requires an explicit type. Noninteractive `--type chatgpt`
uses Codex's default browser login flow, unless `--device-auth` is supplied.
Use `cxa init` to save an existing login without signing in again.

Both ChatGPT and API enrollment accept an optional `--name`:

```sh
cxa add --type chatgpt --name personal
```

```sh
cxa add --type api --name company
```

Without `--name`, both interactive flows end with an account-name prompt.
Press Enter to accept the generated name: the email local part for ChatGPT,
or the API hostname. Noninteractive enrollment accepts the generated name
automatically. Generated names receive `-2`, `-3`, etc. when needed; explicit
names must be unique across both types, ignoring case. Invalid or duplicate
names entered at the prompt can be corrected without repeating login or key
entry. Adding an account saves it without selecting it.

List your accounts and their latest known quota:

```sh
cxa list
```

Switch accounts by their exact name (case-insensitive):

```sh
cxa use personal
```

Names are the only account identifiers: numeric IDs, email addresses, and partial
matches are not accepted. Existing accounts receive persistent names on first
use: ChatGPT names default to the email local part, and API names derive from
their saved label. Collisions receive suffixes such as `-2`. Names do not change
when another account is removed. Credentials remain in their existing storage
directories.

List rows show consecutive display positions starting at 1. Positions are
recomputed after removal and are never accepted by account commands.

Choose a shorter name with:

```sh
cxa rename personal me
```

Names must be unique (case-insensitive), contain 1–64 ASCII letters, digits,
periods, underscores, or hyphens, start with a letter or digit, and cannot be
entirely numeric. Both enrollment types use `--name` or the guided name prompt.

Restart any running Codex or ChatGPT session after switching so it loads the
new account. `cxa` does not restart those processes automatically. For Desktop,
this includes the app-server on a remote development machine when one is used.

### Remove an account

ChatGPT subscriptions and API gateways use the same command and exact names:

```sh
cxa remove work
```

After confirmation, this deletes the account's saved cxa credentials and quota
cache. It does not delete Codex sessions, change the current login/config, or
revoke server tokens or API keys. Re-adding requires login, credential import,
or the API key. Other accounts keep their names.

Selected accounts and accounts still referenced by Codex's credential file
cannot be removed. Switch to another ChatGPT account first. Scripted removal
requires explicit confirmation bypass:

```sh
cxa remove work --yes
```

### Add an API-key account

For guided enrollment, run:

```sh
cxa add --type api
```

Enter the API base URL, API key, and optional service website in that order,
then choose `generic` or `cpa` and an account name. Press Enter to select
`generic`; the name defaults to the API host. Key input is visible in the
terminal; keep screenshots and terminal recordings private. The website is
shown in `cxa status` and `cxa list` and is never visited automatically.

New accounts default to `generic`, which never queries quota or probes a CPA
billing endpoint. To explicitly select CPA and enable its quota queries, run:

```sh
cxa add --type api --api-type cpa
```

Existing CPA profiles created before API types were introduced remain CPA
accounts; no re-enrollment is needed. `cxa` does not infer service types from
URL names, HTTP failures, or missing billing endpoints.

For noninteractive API enrollment, supply `--base-url` and `--api-key-stdin`;
`--name` and `--website` are optional. For example:

```sh
cxa add --type api --name gateway --base-url https://gateway.example.com/v1 --website https://gateway.example.com/dashboard --api-key-stdin < /secure/path/gateway-key.txt
```

### CPA enrollment example

If Codex is already signed in with ChatGPT, enroll that login with `cxa init`
before switching, so its credentials are preserved. Then add a CPA account:

```sh
cxa add --type api --api-type cpa --name company --base-url https://cpa.example.com/v1
```

Enter the CPA key at the prompt, then optionally enter a website. `company` is an arbitrary display name
and the URL above is an example: use your CPA service's actual API URL.
For scripts, add `--api-key-stdin` and supply the key on stdin; do not put the
secret in command arguments.

For example, read a private key file into stdin:

```sh
cxa add --type api --api-type cpa --name company --base-url https://cpa.example.com/v1 --api-key-stdin < /secure/path/cpa-key.txt
```

The path is an example. Keep the file private and do not commit it.

All API gateways must expose the OpenAI Responses API and `/models`; a
Chat Completions-only service is not sufficient. For CPA quota, the service
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
advertised models and switches the connection; `cxa list` queries quota only for ChatGPT and CPA accounts. Neither
the model-list check nor a quota read proves inference compatibility.

The daily commands stay the same:

```sh
cxa list
```

```sh
cxa use company
```

```sh
cxa use personal
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
* 1. personal · Pro 20x
     personal@example.com · updated just now
     Codex
       Weekly   [█████████████░░░]   82% left  resets in 6d 11h
     Codex Spark
       5-hour   [█████████░░░░░░░]   59% left  resets in 4h 12m
       Weekly   [███████████████░]   91% left  resets in 6d 23h

  2. work · Pro 20x
     work@example.com · updated just now
     Codex
       Weekly   [██████░░░░░░░░░░]   37% left  resets in 3d 8h
     Codex Spark
       5-hour   [████████████████]  100% left  resets in 4h 48m
       Weekly   [██████████████░░]   88% left  resets in 5d 17h

  3. company · CPA API key
     https://cpa.example.com/v1 · updated just now
     Core · USD
       Weekly   [██████████████░░]   90% left  resets in 5d 7h
                $360.00 / $400.00 left

$ cxa use work
✓ Account work (work@example.com) is now selected.
! Restart Codex or ChatGPT before expecting an existing session to use this account.
```

The leftmost `*` and a bold green heading mark the account selected in Codex's
credential and connection files. The marker remains visible without colors;
running Codex processes still require a restart after switching. Each
heading shows the display position, unique name, and plan or account type.
The next line shows the email or API endpoint and the quota update time.
The marker and right-aligned numbers form a separate gutter. Account details
and quota groups align with the name column; quota rows are indented two more
spaces. All entries share the same alignment, including lists of 10 or more.
CPA amounts align under the progress bar, keeping the period label in its own column.

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
| `cxa init` | Import the current Codex login |
| `cxa add` | Guided enrollment: choose ChatGPT subscription or API gateway |
| `cxa add --type chatgpt` | Add a ChatGPT subscription; choose browser/device login in a terminal |
| `cxa add --type chatgpt --name NAME` | Add a ChatGPT subscription with a chosen unique name |
| `cxa add --type chatgpt --device-auth` | Add a ChatGPT subscription with device-code login |
| `cxa add --type api` | Guided enrollment with key input and an optional website |
| `cxa add --type api --api-type cpa` | Guided CPA enrollment with quota queries enabled |
| `cxa add --type api --base-url URL --api-key-stdin` | Add a generic API key supplied on stdin; optionally pass `--name NAME` and `--website URL` |
| `cxa models <account>` | Query any API account's advertised models without switching |
| `cxa list` | List accounts and their latest known quota |
| `cxa watch` | Keep the live quota dashboard open |
| `cxa watch --interval 30` | Refresh every 30 seconds |
| `cxa list --watch` | Open watch mode through `list` |
| `cxa use <account>` | Switch by exact account name (case-insensitive) |
| `cxa rename <account> <name>` | Change the unique account name |
| `cxa remove <account>` | Remove a saved account after confirmation |
| `cxa status` | Show the selected account and credential state |
| `cxa relogin <account>` | Re-authenticate a saved account |
| `cxa import <auth.json>` | Import an existing Codex credential file |

Use `cxa --help` or `cxa <command> --help` for the complete CLI reference.

## How it works

Each account is stored as a profile under `~/.codex-auth`. Existing OAuth
profiles keep their original `auth.json` format. API profiles use `api.json`.
Secrets and connection recovery files are written with mode `0600`.

Selecting a ChatGPT account atomically installs its saved OAuth credentials
and explicitly selects the official connection, even if the previous config
pointed at an unmanaged relay. The existing provider ID is never changed.

For an API account using a custom provider, `cxa` keeps the provider ID and
edits only that provider's `name`, `base_url`, `wire_api`, `requires_openai_auth`,
`experimental_bearer_token`, `env_key`, and `auth` settings. The gateway key
is stored in `experimental_bearer_token`, so Desktop can load it without a
shell environment. The provider name is the enrolled API account name,
and `requires_openai_auth` is false. The live OAuth credential file is left
in place. Selecting ChatGPT sets `name = "OpenAI"`, `wire_api = "responses"`,
and `requires_openai_auth = true`, and removes `base_url`, `env_key`,
`experimental_bearer_token`, and `auth`. It also removes a stale top-level
`openai_base_url`, so the subscription uses the official default endpoint.
Old connection snapshots are not restored. Isolated ChatGPT login and quota
queries use this same official connection without changing the live config.
Models, reasoning settings, MCP servers, projects, unrelated provider tuning,
and TOML comments remain in the live config; edits to those settings while
using the API account are retained.

For the built-in `openai` provider, `cxa` changes only `openai_base_url` in the
config and installs API-key credentials in `auth.json`. The current OAuth
login must be enrolled first, and its latest credentials are saved before the
switch. Selecting ChatGPT removes `openai_base_url` and installs its saved
OAuth credentials, rather than restoring a possibly stale relay URL.
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

For CPA accounts, `cxa` uses the saved key to query
`/v0/resource/plugins/cpa-key-billing/subscription`. It removes a trailing
`/v1` from the base URL while retaining any reverse-proxy path prefix. It does
not follow HTTP redirects. Generic API accounts use the same switching
mechanism but never query quota. Optional service websites are display-only.
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
