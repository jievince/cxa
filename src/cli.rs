use std::ffi::OsString;
use std::fmt::Write as _;
use std::io::{self, IsTerminal, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use anstream::{eprintln, print, println};
use clap::{Parser, Subcommand, ValueEnum};
use tempfile::Builder;

use crate::account_store::{Account, Profile, Store, UsageRecord, now_epoch};
use crate::api_account::{ApiAccount, ApiType};
use crate::app_server::{CancellationToken, query_profile_cancellable, require_file_credentials};
use crate::auth::AuthDocument;
use crate::config::Config;
use crate::fs::{ExclusiveLock, private_dir, remove_file_if_exists};
use crate::terminal::{
    ACCENT, EMPHASIS, FetchSpinner, LiveRegion, MUTED, SUCCESS, WARNING, WatchTerminal,
    render_usage, usage_plan, usage_recency, watch_exit_requested,
};
use crate::{Error, Result};
use crate::{api_models, connection, cpa};

#[derive(Debug, Parser)]
#[command(
    name = "cxa",
    version,
    about = "Switch ChatGPT and API-key accounts in Codex",
    long_about = "Switch between ChatGPT subscription accounts and Responses-compatible API-key accounts in Codex CLI and Desktop.\n\nKeep the same Codex home and model_provider. ChatGPT switches replace file credentials; API-key switches also update connection settings without replacing unrelated config.toml settings. Existing Codex processes must be restarted after switching.\n\nUse cxa init to save the current ChatGPT login, cxa add for guided ChatGPT or API enrollment; --type chatgpt and --type api skip the type prompt, cxa list to show accounts and available quota, and cxa models ACCOUNT to query gateway model IDs. Generic API accounts do not query quota; select --api-type cpa to enable CPA subscription queries."
)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,
    #[arg(value_name = "NAME")]
    account: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum EnrollmentType {
    Chatgpt,
    Api,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    /// Import the current Codex login and select it as the first account.
    Init {
        /// Import without asking for confirmation.
        #[arg(long)]
        yes: bool,
    },
    /// List enrolled accounts and their remaining quota.
    List {
        /// Keep the live list open and refresh it periodically.
        #[arg(short, long)]
        watch: bool,
        /// Seconds between refreshes in watch mode.
        #[arg(
            long,
            default_value_t = 60,
            requires = "watch",
            value_parser = clap::value_parser!(u64).range(5..)
        )]
        interval: u64,
    },
    /// Keep the live remaining-quota dashboard open.
    Watch {
        /// Seconds between refreshes.
        #[arg(
            long,
            default_value_t = 60,
            value_parser = clap::value_parser!(u64).range(5..)
        )]
        interval: u64,
    },
    /// Show the selected account and credential-file state.
    Status,
    /// Switch by the account's exact name (case-insensitive).
    Use { account: String },
    /// Change an account's unique name without changing its credentials.
    Rename { account: String, name: String },
    /// Remove a saved account and quota cache, not Codex sessions or server credentials.
    Remove {
        account: String,
        /// Remove without asking for confirmation; active credentials remain protected.
        #[arg(long)]
        yes: bool,
    },
    /// List an API account's advertised model IDs without switching accounts.
    Models {
        /// API account, or the selected account when omitted.
        account: Option<String>,
    },
    /// Enroll a ChatGPT OAuth account or a Responses-compatible API key.
    #[command(
        trailing_var_arg = true,
        long_about = "Enroll a ChatGPT subscription or a Responses-compatible API gateway.\n\nRun cxa add to choose an account type in the interactive wizard. Use --type chatgpt or --type api to skip the type prompt. Redirected input requires an explicit type (or --device-auth for ChatGPT).\n\nChatGPT enrollment runs Codex login in an isolated home and saves the resulting OAuth credentials without changing your current login. In a terminal, choose browser or device-code login; --device-auth selects device-code login directly. Noninteractive --type chatgpt uses Codex's default browser login flow.\n\nRun cxa add --type api to enter the base URL, API key, optional website, then choose generic/cpa and an account name. These can also be supplied with --base-url, --website, --name, and --api-key-stdin. Scripted API enrollment requires --type api, --base-url, --name, and --api-key-stdin.\n\nNew API accounts default to generic: quota queries are disabled. Select --api-type cpa to query CLIProxyAPI (CPA) quota with GET /v0/resource/plugins/cpa-key-billing/subscription, using the enrolled key as a Bearer token. cxa does not install or load server plugins and does not guess a service type from HTTP failures. All API gateways must support the OpenAI Responses API and /models.\n\nAPI enrollment only saves the key locally: it does not verify the key or change the current login or config.toml. Use cxa use ACCOUNT to check advertised models and switch; use cxa list to show accounts, websites, and supported quota."
    )]
    Add {
        /// Sign in with Codex's device-code flow.
        #[arg(long)]
        device_auth: bool,
        /// Account type; when omitted, choose in the interactive wizard.
        #[arg(long = "type", value_enum)]
        kind: Option<EnrollmentType>,
        /// Service type: generic (default, no quota query) or cpa.
        #[arg(long, value_enum, requires = "kind")]
        api_type: Option<ApiType>,
        /// Unique name for the API account (letters, digits, dots, underscores, hyphens).
        #[arg(long, requires = "kind")]
        name: Option<String>,
        /// API base URL, e.g. https://gateway.example.com/v1.
        #[arg(long, requires = "kind")]
        base_url: Option<String>,
        /// Optional service website, shown in status/list for manual quota checks.
        #[arg(long, requires = "kind")]
        website: Option<String>,
        /// Read the API key from stdin instead of an interactive prompt.
        #[arg(long, requires = "kind")]
        api_key_stdin: bool,
        #[arg(allow_hyphen_values = true, hide = true)]
        options: Vec<OsString>,
    },
    /// Import an existing Codex auth.json file.
    Import { auth_file: PathBuf },
    /// Re-authenticate an existing account.
    #[command(trailing_var_arg = true)]
    Relogin {
        account: String,
        #[arg(allow_hyphen_values = true)]
        options: Vec<OsString>,
    },
}

pub fn run(cli: Cli, config: Config) -> Result<()> {
    if config.account_store.join("switch-pending.json").exists() {
        let store = Store::new(config.clone());
        let _lock = store.lock()?;
        connection::recover(&config)?;
    }
    require_file_credentials(&config)?;
    let app = App::new(config);
    match (cli.command, cli.account) {
        (Some(CliCommand::Init { yes }), _) => app.init(yes),
        (Some(CliCommand::List { watch, interval }), _) => app.list(watch, interval),
        (Some(CliCommand::Watch { interval }), _) => app.list(true, interval),
        (Some(CliCommand::Status), _) | (None, None) => app.status(true),
        (Some(CliCommand::Use { account }), _) | (None, Some(account)) => app.switch(&account),
        (Some(CliCommand::Rename { account, name }), _) => app.rename(&account, &name),
        (Some(CliCommand::Remove { account, yes }), _) => app.remove(&account, yes),
        (Some(CliCommand::Models { account }), _) => app.models(account.as_deref()),
        (
            Some(CliCommand::Add {
                device_auth,
                kind,
                api_type,
                name,
                base_url,
                website,
                api_key_stdin,
                options,
            }),
            _,
        ) => {
            if options.iter().any(|option| option == "--api-key") {
                return Err(Error::Message("The account-type flag is now --type api, not --api-key. Use cxa add for the guided flow.".into()));
            }
            let kind = match kind {
                Some(kind) => kind,
                None if device_auth || !options.is_empty() => EnrollmentType::Chatgpt,
                None => {
                    if !io::stdin().is_terminal() {
                        return Err(Error::Message("Choose an account type with --type chatgpt or --type api when input is redirected.".into()));
                    }
                    println!(
                        "Add an account:\n  1  ChatGPT subscription\n  2  API gateway (Responses-compatible)"
                    );
                    match prompt_line("Account type [1/2]: ")?.as_str() {
                        "1" | "chatgpt" => EnrollmentType::Chatgpt,
                        "2" | "api" => EnrollmentType::Api,
                        _ => return Err(Error::Message(
                            "Account type must be 1 (chatgpt) or 2 (api); no account was saved."
                                .into(),
                        )),
                    }
                }
            };
            if kind == EnrollmentType::Api {
                if device_auth || !options.is_empty() {
                    return Err(Error::Message(
                        "Codex login arguments cannot be used with --type api.".into(),
                    ));
                }
                app.add_api(name, base_url, website, api_type, api_key_stdin)
            } else {
                if api_type.is_some()
                    || name.is_some()
                    || base_url.is_some()
                    || website.is_some()
                    || api_key_stdin
                {
                    return Err(Error::Message(
                        "API enrollment options require --type api; no account was saved.".into(),
                    ));
                }
                let device_auth = if !device_auth && options.is_empty() && io::stdin().is_terminal()
                {
                    println!(
                        "ChatGPT login runs in an isolated Codex home; your current login is not changed."
                    );
                    match prompt_line("Login method [browser/device, Enter=browser]: ")?.as_str() {
                        "" | "browser" => false,
                        "device" => true,
                        _ => {
                            return Err(Error::Message(
                                "Login method must be browser or device; no account was saved."
                                    .into(),
                            ));
                        }
                    }
                } else {
                    device_auth
                };
                app.add(device_auth, &options)
            }
        }
        (Some(CliCommand::Import { auth_file }), _) => app.import(&auth_file),
        (Some(CliCommand::Relogin { account, options }), _) => app.relogin(&account, &options),
    }
}

struct App {
    store: Store,
}

impl App {
    fn new(config: Config) -> Self {
        Self {
            store: Store::new(config),
        }
    }

    fn locked(&self) -> Result<ExclusiveLock> {
        let lock = self.store.lock()?;
        connection::recover(&self.store.config)?;
        self.store.ensure_names()?;
        self.store.sync_session_profile()?;
        Ok(lock)
    }

    fn try_locked(&self) -> Result<Option<ExclusiveLock>> {
        let Some(lock) = self.store.try_lock()? else {
            return Ok(None);
        };
        connection::recover(&self.store.config)?;
        self.store.ensure_names()?;
        self.store.sync_session_profile()?;
        Ok(Some(lock))
    }

    fn init(&self, assume_yes: bool) -> Result<()> {
        let _lock = self.locked()?;
        let profiles = self.store.profiles()?;
        if !profiles.is_empty() {
            if let Some(selected) = self.store.selected()? {
                let profile = self.store.profile(selected)?;
                println!(
                    "{SUCCESS}✓{SUCCESS:#} cxa is already initialized with account {ACCENT}{}{ACCENT:#} ({EMPHASIS}{}{EMPHASIS:#}).",
                    profile.name,
                    profile.label()
                );
                return Ok(());
            }
            return Err(Error::Message(
                "cxa has enrolled accounts but none is selected. Run `cxa list`, then `cxa <account>`."
                    .into(),
            ));
        }

        let session_path = &self.store.config.session_auth;
        if !session_path.is_file() {
            return Err(Error::Message(
                "No current Codex login was found. Run `codex login`, then run `cxa init`.".into(),
            ));
        }
        let current = AuthDocument::read(session_path)?;
        if !assume_yes {
            if !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
                return Err(Error::Message(format!(
                    "Found the current Codex login: {}. Run `cxa init --yes` when input or output is redirected.",
                    current.identity.label()
                )));
            }
            print!(
                "{ACCENT}?{ACCENT:#} Found the current Codex login: {EMPHASIS}{}{EMPHASIS:#}\n  Import this account? {MUTED}[Y/n]{MUTED:#} ",
                current.identity.label()
            );
            io::stdout()
                .flush()
                .map_err(|error| Error::io("stdout", error))?;
            let mut answer = String::new();
            io::stdin()
                .read_line(&mut answer)
                .map_err(|error| Error::io("stdin", error))?;
            if matches!(answer.trim().to_ascii_lowercase().as_str(), "n" | "no") {
                return Err(Error::Message("Initialization cancelled.".into()));
            }
        }

        let profile = self.store.enroll(session_path)?;
        println!(
            "{SUCCESS}✓{SUCCESS:#} Imported {EMPHASIS}{}{EMPHASIS:#} as account {ACCENT}{}{ACCENT:#}.",
            profile.label(),
            profile.name
        );
        println!(
            "{SUCCESS}✓{SUCCESS:#} Account {ACCENT}{}{ACCENT:#} is now selected.",
            profile.name
        );
        println!("{MUTED}Next:{MUTED:#} add another account with {ACCENT}cxa add{ACCENT:#}");
        Ok(())
    }

    fn list(&self, watch: bool, interval: u64) -> Result<()> {
        let mut region = LiveRegion::new();
        if watch && !(region.is_active() && io::stdin().is_terminal()) {
            return Err(Error::Message(
                "Watch mode requires an interactive terminal.".into(),
            ));
        }
        if !watch {
            let ListRefresh::Completed { session_changed } =
                self.refresh_list(&mut region, false, false)?
            else {
                unreachable!("one-shot lists do not read keyboard input");
            };
            if session_changed {
                restart_notice();
            }
            return Ok(());
        }

        let _terminal = WatchTerminal::enter().map_err(|error| Error::io("terminal", error))?;
        let mut restart_required = false;
        loop {
            match self.refresh_list(&mut region, true, true)? {
                ListRefresh::Completed { session_changed } => {
                    restart_required |= session_changed;
                }
                ListRefresh::ExitRequested => return Ok(()),
            }
            for remaining in (1..=interval).rev() {
                let status = watch_status(remaining, restart_required, region.width());
                region
                    .write_status(&status)
                    .map_err(|error| Error::io("stdout", error))?;
                if watch_exit_requested(Duration::from_secs(1))
                    .map_err(|error| Error::io("terminal input", error))?
                {
                    return Ok(());
                }
            }
        }
    }

    fn refresh_list(
        &self,
        region: &mut LiveRegion,
        force_refresh: bool,
        watch: bool,
    ) -> Result<ListRefresh> {
        let _lock = if watch {
            loop {
                if let Some(lock) = self.try_locked()? {
                    break lock;
                }
                if watch_exit_requested(Duration::from_millis(80))
                    .map_err(|error| Error::io("terminal input", error))?
                {
                    return Ok(ListRefresh::ExitRequested);
                }
            }
        } else {
            self.locked()?
        };
        let profiles = self.store.profiles()?;
        if profiles.is_empty() {
            let output = format!("{}\n", self.initialization_guidance());
            region
                .redraw(&output)
                .map_err(|error| Error::io("stdout", error))?;
            return Ok(ListRefresh::Completed {
                session_changed: false,
            });
        }
        let selected = self.store.selected()?;
        let mut states: Vec<ProfileUsage> = profiles
            .iter()
            .map(|profile| {
                if profile.supports_usage()
                    && !self.store.config.skip_usage_refresh
                    && (force_refresh || self.needs_usage_refresh(profile.slot))
                {
                    ProfileUsage::Loading
                } else {
                    ProfileUsage::Ready(self.store.usage(profile.slot))
                }
            })
            .collect();
        let refresh_slots: Vec<u32> = profiles
            .iter()
            .zip(&states)
            .filter_map(|(profile, state)| state.is_loading().then_some(profile.slot))
            .collect();
        let mut frame = 0;
        if region.is_active() && !refresh_slots.is_empty() {
            let output = profile_list(&profiles, selected, &states, now_epoch(), frame);
            region
                .redraw(&output)
                .map_err(|error| Error::io("stdout", error))?;
        }

        let cancellation = CancellationToken::default();
        let (sender, receiver) = mpsc::channel();
        let mut workers = UsageWorkers::new(cancellation.clone());
        for slot in refresh_slots {
            let sender = sender.clone();
            let config = self.store.config.clone();
            let cancellation = cancellation.clone();
            workers.push(thread::spawn(move || {
                let result = refresh_usage_for_config(&config, slot, force_refresh, cancellation);
                let _ = sender.send((slot, result));
            }));
        }
        drop(sender);

        let mut session_changed = false;
        let mut first_error = None;
        let refresh_total = workers.len();
        let mut completed = 0;
        let mut exit_requested = false;
        while completed < refresh_total {
            let received = if region.is_active() {
                match receiver.recv_timeout(Duration::from_millis(80)) {
                    Ok(result) => Some(result),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            } else {
                receiver.recv().ok()
            };
            if let Some((slot, result)) = received {
                completed += 1;
                match result {
                    Ok(changed) => session_changed |= changed,
                    Err(error) if first_error.is_none() => first_error = Some(error),
                    Err(_) => {}
                }
                if let Some((index, _)) = profiles
                    .iter()
                    .enumerate()
                    .find(|(_, profile)| profile.slot == slot)
                {
                    states[index] = ProfileUsage::Ready(self.store.usage(slot));
                }
            }
            if region.is_active() {
                frame += 1;
                let output = profile_list(&profiles, selected, &states, now_epoch(), frame);
                region
                    .redraw(&output)
                    .map_err(|error| Error::io("stdout", error))?;
            }
            if watch
                && watch_exit_requested(Duration::ZERO)
                    .map_err(|error| Error::io("terminal input", error))?
            {
                cancellation.cancel();
                exit_requested = true;
                break;
            }
        }
        if workers.join_panicked() && first_error.is_none() {
            first_error = Some(Error::Message("usage refresh worker failed".into()));
        }
        if exit_requested {
            return Ok(ListRefresh::ExitRequested);
        }
        let output = profile_list(&profiles, selected, &states, now_epoch(), frame);
        if region.is_active() {
            region
                .redraw(&output)
                .map_err(|error| Error::io("stdout", error))?;
        } else {
            print!("{output}");
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(ListRefresh::Completed { session_changed })
    }

    fn status(&self, refresh: bool) -> Result<()> {
        let _lock = self.locked()?;
        if self.store.profiles()?.is_empty() {
            println!("{}", self.initialization_guidance());
            return Ok(());
        }
        let selected = self
            .store
            .selected()?
            .ok_or_else(|| Error::Message("No Codex account is selected.".into()))?;
        let profile = self.store.profile(selected)?;
        let session_changed =
            if profile.supports_usage() && refresh && self.needs_usage_refresh(selected) {
                let spinner =
                    FetchSpinner::start(format!("Fetching usage [1/1] {}", profile.label()));
                let changed = self.refresh_usage(selected)?;
                spinner.finish();
                changed
            } else {
                false
            };
        let usage = self.store.usage(selected);
        print!(
            "{}",
            profile_output(&profile, true, usage.as_ref(), now_epoch())
        );
        let credential = self.store.credential_status(selected)?;
        let style = if credential.starts_with("matches") {
            SUCCESS
        } else {
            WARNING
        };
        println!("    {ACCENT}Credential{ACCENT:#}  {style}{credential}{style:#}");
        if session_changed {
            restart_notice();
        }
        Ok(())
    }

    fn initialization_guidance(&self) -> String {
        match AuthDocument::read(&self.store.config.session_auth) {
            Ok(auth) => format!(
                "Found the current Codex login: {}\ncxa is not initialized. Run: cxa init",
                auth.identity.label()
            ),
            Err(_) => "cxa is not initialized. Run `codex login`, then run `cxa init`.".into(),
        }
    }

    fn refresh_usage(&self, slot: u32) -> Result<bool> {
        refresh_usage_for_config(
            &self.store.config,
            slot,
            false,
            CancellationToken::default(),
        )
    }

    fn needs_usage_refresh(&self, slot: u32) -> bool {
        !self.store.config.skip_usage_refresh && !self.store.usage_fresh(slot)
    }

    fn switch(&self, selector: &str) -> Result<()> {
        let _lock = self.locked()?;
        let target = self.store.resolve(selector)?;
        let gateway_models = if let Account::Api(account) = &target.account {
            let models = api_models::query(account).map_err(|error| {
                Error::Message(format!("{error} No account or connection was switched."))
            })?;
            if let Some(model) = connection::configured_model(&self.store.config)?
                .filter(|model| !models.contains(model))
            {
                return Err(Error::Message(format!(
                    "Default model `{model}` in config.toml is not advertised by this API gateway. Available models: {}. Choose a supported model explicitly; no account, auth.json, or config.toml was changed.",
                    models.join(", ")
                )));
            }
            Some(models)
        } else {
            None
        };
        if target.supports_usage()
            && self
                .store
                .usage(target.slot)
                .is_some_and(|usage| usage.exhausted_now(now_epoch()))
        {
            eprintln!(
                "{WARNING}warning{WARNING:#}: account {ACCENT}{}{ACCENT:#} ({EMPHASIS}{}{EMPHASIS:#}) was last seen exhausted",
                target.name,
                target.label()
            );
        }
        let selected = self.store.select(target.slot)?;
        println!(
            "{SUCCESS}✓{SUCCESS:#} Account {ACCENT}{}{ACCENT:#} ({EMPHASIS}{}{EMPHASIS:#}) is now selected.",
            selected.name,
            selected.label()
        );
        restart_notice();
        if let Some(models) = gateway_models {
            println!("{MUTED}Gateway models:{MUTED:#} {}", models.join(", "));
            println!(
                "{WARNING}!{WARNING:#} Desktop and existing chats can select a different model than config.toml. Select an advertised model in that chat; cxa does not change chat models or the model picker."
            );
        }
        Ok(())
    }

    fn models(&self, selector: Option<&str>) -> Result<()> {
        let _lock = self.locked()?;
        let profile = match selector {
            Some(selector) => self.store.resolve(selector)?,
            None => self.store.profile(
                self.store
                    .selected()?
                    .ok_or_else(|| Error::Message("No account is selected.".into()))?,
            )?,
        };
        let Account::Api(account) = &profile.account else {
            return Err(Error::Message(
                "`cxa models` queries API gateways only. Use Codex's model picker for ChatGPT accounts.".into(),
            ));
        };
        let models = api_models::query(account)?;
        println!("Gateway models for {}:", profile.label());
        for model in models {
            println!("  {model}");
        }
        Ok(())
    }

    fn rename(&self, selector: &str, name: &str) -> Result<()> {
        let _lock = self.locked()?;
        let profile = self.store.rename(selector, name)?;
        println!(
            "✓ Account {} ({}) is now named {}. Credentials and config.toml were not changed.",
            selector,
            profile.label(),
            profile.name
        );
        Ok(())
    }

    fn add(&self, device_auth: bool, options: &[OsString]) -> Result<()> {
        let mut options = options.to_vec();
        if device_auth {
            options.push("--device-auth".into());
        }
        reject_non_oauth(&options)?;
        let _lock = self.locked()?;
        let login = StagedLogin::run(&self.store.config, &options)?;
        let profile = self.store.enroll(&login.auth_path())?;
        println!(
            "\n{SUCCESS}✓{SUCCESS:#} Enrolled {EMPHASIS}{}{EMPHASIS:#} as account {ACCENT}{}{ACCENT:#}.",
            profile.label(),
            profile.name
        );
        println!(
            "{MUTED}Next:{MUTED:#} switch to it with {ACCENT}cxa use {}{ACCENT:#}",
            profile.name
        );
        Ok(())
    }

    fn remove(&self, selector: &str, assume_yes: bool) -> Result<()> {
        let _lock = self.locked()?;
        let profile = self.store.resolve(selector)?;
        self.store.check_removable(&profile)?;
        if !assume_yes {
            if !io::stdin().is_terminal() {
                return Err(Error::Message("Removing an account requires confirmation; use --yes when input is redirected.".into()));
            }
            println!(
                "Remove account {} ({})? This deletes its saved credentials and quota cache. Re-adding requires login or the API key. Codex sessions and server credentials are not removed.",
                profile.name,
                profile.label()
            );
            if prompt_line("Remove this account? [y/N]: ")? != "y" {
                println!("Cancelled; nothing was removed.");
                return Ok(());
            }
        }
        self.store.remove(&profile)?;
        println!(
            "✓ Removed account {} ({}) from cxa. Codex sessions, login, and config.toml were not changed.",
            profile.name,
            profile.label()
        );
        Ok(())
    }

    fn add_api(
        &self,
        name: Option<String>,
        base_url: Option<String>,
        website: Option<String>,
        api_type: Option<ApiType>,
        from_stdin: bool,
    ) -> Result<()> {
        let interactive = !from_stdin && io::stdin().is_terminal();
        if !interactive && (name.is_none() || base_url.is_none()) {
            return Err(Error::Message(
                "Scripted API enrollment requires --name and --base-url; use cxa add --type api in a terminal for interactive enrollment.".into(),
            ));
        }
        let base_url = match base_url {
            Some(url) => url,
            None => prompt_line("API base URL: ")?,
        };
        let key = if from_stdin {
            let mut key = String::new();
            io::stdin()
                .read_line(&mut key)
                .map_err(|error| Error::io("stdin", error))?;
            key
        } else {
            if !io::stdin().is_terminal() {
                return Err(Error::Message(
                    "Use --api-key-stdin when API-key input is redirected.".into(),
                ));
            }
            prompt_line("API key: ")?
        };
        let website = match website {
            Some(url) => Some(url),
            None if interactive => {
                Some(prompt_line("Service website (optional, Enter to skip): ")?)
            }
            None => None,
        };
        let api_type = match api_type {
            Some(api_type) => api_type,
            None if interactive => {
                match prompt_line("Service type [generic/cpa, Enter=generic]: ")?.as_str() {
                    "" | "generic" => ApiType::Generic,
                    "cpa" => ApiType::Cpa,
                    _ => {
                        return Err(Error::Message(
                            "Service type must be generic or cpa; no account was saved.".into(),
                        ));
                    }
                }
            }
            None => ApiType::Generic,
        };
        eprintln!(
            "API type: {}. {}",
            api_type.label(),
            match api_type {
                ApiType::Generic =>
                    "Quota queries are disabled; an optional website is shown for manual checks.",
                ApiType::Cpa =>
                    "Quota queries use the CPA subscription endpoint (GET + Bearer authentication).",
            }
        );
        let name = match name {
            Some(name) => name,
            None => {
                let default = reqwest::Url::parse(base_url.trim())
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_owned))
                    .unwrap_or_else(|| "api".into());
                let entered = prompt_line(&format!("Account name [{default}]: "))?;
                if entered.is_empty() { default } else { entered }
            }
        };
        let account = ApiAccount::new(name, base_url, key, api_type, website)?;
        let _lock = self.locked()?;
        let profile = self.store.enroll_api(account)?;
        println!(
            "{SUCCESS}✓{SUCCESS:#} Enrolled {EMPHASIS}{}{EMPHASIS:#} as account {ACCENT}{}{ACCENT:#} ({}).",
            profile.label(),
            profile.name,
            api_type.label()
        );
        println!(
            "{MUTED}Saved locally; the key has not been verified. Your current login and config.toml were not changed.{MUTED:#}"
        );
        println!(
            "{MUTED}Next:{MUTED:#} check advertised models and switch with {ACCENT}cxa use {}{ACCENT:#}; show accounts with {ACCENT}cxa list{ACCENT:#}.",
            profile.name
        );
        Ok(())
    }

    fn import(&self, auth_file: &Path) -> Result<()> {
        let _lock = self.locked()?;
        let profile = self.store.enroll(auth_file)?;
        println!(
            "{SUCCESS}✓{SUCCESS:#} Imported {EMPHASIS}{}{EMPHASIS:#} as account {ACCENT}{}{ACCENT:#}.",
            profile.label(),
            profile.name
        );
        println!(
            "{MUTED}Next:{MUTED:#} switch to it with {ACCENT}cxa use {}{ACCENT:#}",
            profile.name
        );
        Ok(())
    }

    fn relogin(&self, selector: &str, options: &[OsString]) -> Result<()> {
        reject_non_oauth(options)?;
        let _lock = self.locked()?;
        let target = self.store.resolve(selector)?;
        target.oauth()?;
        println!(
            "{ACCENT}Re-authenticating account {}{ACCENT:#} ({EMPHASIS}{}{EMPHASIS:#}).\n",
            target.name,
            target.label()
        );
        let login = StagedLogin::run(&self.store.config, options)?;
        let profile = self.store.replace(target.slot, &login.auth_path())?;
        remove_file_if_exists(&self.store.config.profile_usage(target.slot))?;
        if self.store.selected()? == Some(target.slot) {
            self.store.select(target.slot)?;
            restart_notice();
        }
        println!(
            "{SUCCESS}✓{SUCCESS:#} Re-authenticated {EMPHASIS}{}{EMPHASIS:#} as account {ACCENT}{}{ACCENT:#}.",
            profile.label(),
            profile.name
        );
        Ok(())
    }
}

enum ListRefresh {
    Completed { session_changed: bool },
    ExitRequested,
}

enum ProfileUsage {
    Loading,
    Ready(Option<UsageRecord>),
}

struct UsageWorkers {
    cancellation: CancellationToken,
    handles: Vec<thread::JoinHandle<()>>,
}

impl UsageWorkers {
    fn new(cancellation: CancellationToken) -> Self {
        Self {
            cancellation,
            handles: Vec::new(),
        }
    }

    fn push(&mut self, worker: thread::JoinHandle<()>) {
        self.handles.push(worker);
    }

    fn len(&self) -> usize {
        self.handles.len()
    }

    fn cancel(&self) {
        self.cancellation.cancel();
    }

    fn join_panicked(&mut self) -> bool {
        let mut panicked = false;
        for worker in self.handles.drain(..) {
            panicked |= worker.join().is_err();
        }
        panicked
    }
}

impl Drop for UsageWorkers {
    fn drop(&mut self) {
        self.cancel();
        let _ = self.join_panicked();
    }
}

impl ProfileUsage {
    fn is_loading(&self) -> bool {
        matches!(self, Self::Loading)
    }
}

const LOADING_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn watch_status(remaining: u64, restart_required: bool, width: usize) -> String {
    let remaining = interval_label(remaining);
    let candidates = if restart_required {
        vec![
            format!("! Restart Codex/ChatGPT · refresh in {remaining} · q/Ctrl-C to exit"),
            format!("! Restart Codex · {remaining} · q to exit"),
            format!("! {remaining} · q"),
            remaining,
        ]
    } else {
        vec![
            format!("Watching · refresh in {remaining} · q/Ctrl-C to exit"),
            format!("Watching · {remaining} · q to exit"),
            format!("{remaining} · q"),
            remaining,
        ]
    };
    let text = candidates
        .iter()
        .find(|candidate| candidate.chars().count() <= width)
        .cloned()
        .unwrap_or_else(|| candidates.last().unwrap().chars().take(width).collect());
    let style = if restart_required { WARNING } else { MUTED };
    format!("{style}{text}{style:#}")
}

fn interval_label(seconds: u64) -> String {
    if seconds >= 60 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

fn profile_list(
    profiles: &[Profile],
    selected: Option<u32>,
    states: &[ProfileUsage],
    now: i64,
    frame: usize,
) -> String {
    let mut output = String::new();
    for (index, (profile, state)) in profiles.iter().zip(states).enumerate() {
        if index > 0 {
            writeln!(output).unwrap();
        }
        match state {
            ProfileUsage::Loading => {
                write_loading_profile(&mut output, profile, selected == Some(profile.slot), frame);
            }
            ProfileUsage::Ready(usage) => {
                output.push_str(&profile_output(
                    profile,
                    selected == Some(profile.slot),
                    usage.as_ref(),
                    now,
                ));
            }
        }
    }
    output
}

fn write_loading_profile(output: &mut String, profile: &Profile, selected: bool, frame: usize) {
    let marker = if selected { "*" } else { " " };
    let spinner = LOADING_FRAMES[frame % LOADING_FRAMES.len()];
    writeln!(
        output,
        "{ACCENT}{marker}{ACCENT:#} {EMPHASIS}{}{EMPHASIS:#}  {ACCENT}{spinner}{ACCENT:#} {MUTED}loading{MUTED:#}",
        profile.heading()
    )
    .unwrap();
}

fn refresh_usage_for_config(
    config: &Config,
    slot: u32,
    force_refresh: bool,
    cancellation: CancellationToken,
) -> Result<bool> {
    let store = Store::new(config.clone());
    if config.skip_usage_refresh || (!force_refresh && store.usage_fresh(slot)) {
        return Ok(false);
    }
    let previous = store.usage(slot);
    let (next, session_changed) = match store.profile(slot)?.account {
        Account::Chatgpt(_) => {
            query_profile_cancellable(config, &config.profile_auth(slot), cancellation)?
        }
        Account::Api(account) => match account.api_type {
            ApiType::Generic => return Ok(false),
            ApiType::Cpa => (cpa::query(&account, cancellation)?, false),
        },
    };
    write_usage_result(previous.as_ref(), &next, &config.profile_usage(slot))?;
    Ok(session_changed)
}

fn profile_output(
    profile: &Profile,
    selected: bool,
    usage: Option<&UsageRecord>,
    now: i64,
) -> String {
    let mut output = String::new();
    let marker = if selected { "*" } else { " " };
    let plan = match &profile.account {
        Account::Chatgpt(_) => usage_plan(usage),
        Account::Api(account) => Some(account.api_type.label().into()),
    };
    let recency = if profile.supports_usage() {
        usage_recency(usage, now)
    } else {
        None
    };
    if let (Some(plan), Some(recency)) = (plan.as_deref(), recency.as_deref()) {
        writeln!(
            output,
            "{ACCENT}{marker}{ACCENT:#} {EMPHASIS}{}{EMPHASIS:#}  {MUTED}{plan} · {recency}{MUTED:#}",
            profile.heading()
        )
        .unwrap();
    } else if let Some(plan) = plan {
        writeln!(
            output,
            "{ACCENT}{marker}{ACCENT:#} {EMPHASIS}{}{EMPHASIS:#}  {MUTED}{plan}{MUTED:#}",
            profile.heading()
        )
        .unwrap();
    } else if let Some(recency) = recency {
        writeln!(
            output,
            "{ACCENT}{marker}{ACCENT:#} {EMPHASIS}{}{EMPHASIS:#}  {MUTED}{recency}{MUTED:#}",
            profile.heading()
        )
        .unwrap();
    } else {
        writeln!(
            output,
            "{ACCENT}{marker}{ACCENT:#} {EMPHASIS}{}{EMPHASIS:#}",
            profile.heading()
        )
        .unwrap();
    }
    if let Account::Api(account) = &profile.account {
        if let Some(website) = &account.website {
            writeln!(output, "    Website  {website}").unwrap();
        }
    }
    if profile.supports_usage() {
        output.push_str(&render_usage(usage, now));
    } else {
        writeln!(
            output,
            "    {MUTED}Quota query disabled (generic API).{MUTED:#}"
        )
        .unwrap();
    }
    output
}

fn prompt_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout()
        .flush()
        .map_err(|error| Error::io("stdout", error))?;
    let mut input = String::new();
    if io::stdin()
        .read_line(&mut input)
        .map_err(|error| Error::io("stdin", error))?
        == 0
    {
        return Err(Error::Message(
            "Account enrollment cancelled: input ended.".into(),
        ));
    }
    Ok(input.trim().to_owned())
}

fn restart_notice() {
    println!(
        "{WARNING}!{WARNING:#} Restart Codex or ChatGPT before expecting an existing session to use this account."
    );
}

fn write_usage_result(
    previous: Option<&UsageRecord>,
    next: &UsageRecord,
    path: &Path,
) -> Result<()> {
    if !next.succeeded() {
        if let Some(previous) = previous.filter(|usage| usage.succeeded()) {
            let mut retained = previous.clone();
            retained.last_attempted_at = next.last_attempted_at.max(next.observed_at);
            retained.refresh_error = next.error.clone();
            return retained.write(path);
        }
    }
    next.write(path)
}

fn reject_non_oauth(options: &[OsString]) -> Result<()> {
    for option in options {
        let Some(option) = option.to_str() else {
            continue;
        };
        let name = option.split_once('=').map_or(option, |(name, _)| name);
        if matches!(name, "--with-api-key" | "--with-access-token") {
            return Err(Error::Message(format!(
                "cxa requires ChatGPT OAuth credentials; {name} is not supported."
            )));
        }
    }
    Ok(())
}

struct StagedLogin {
    home: tempfile::TempDir,
}

impl StagedLogin {
    fn run(config: &Config, options: &[OsString]) -> Result<Self> {
        private_dir(&config.account_store)?;
        let home = Builder::new()
            .prefix(".login-")
            .tempdir_in(&config.account_store)
            .map_err(|error| Error::io(&config.account_store, error))?;
        connection::copy_oauth_config(config, &home.path().join("config.toml"))?;
        let status = Command::new(config.codex_binary())
            .arg("login")
            .args(options)
            .args(["-c", "cli_auth_credentials_store=\"file\""])
            .env("CODEX_HOME", home.path())
            .status()
            .map_err(|error| Error::io("codex login", error))?;
        if !status.success() || !home.path().join("auth.json").is_file() {
            return Err(Error::Message(
                "Codex login failed; nothing was changed.".into(),
            ));
        }
        Ok(Self { home })
    }

    fn auth_path(&self) -> PathBuf {
        self.home.path().join("auth.json")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_refresh_preserves_successful_usage() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("usage.json");
        let previous = UsageRecord {
            observed_at: 1,
            last_attempted_at: 1,
            ..UsageRecord::default()
        };
        previous.write(&path).unwrap();
        let failed = UsageRecord {
            observed_at: 2,
            last_attempted_at: 2,
            error: Some("quota unavailable (Timeout)".into()),
            ..UsageRecord::default()
        };

        write_usage_result(Some(&previous), &failed, &path).unwrap();

        let retained = UsageRecord::read(&path).unwrap();
        assert_eq!(retained.observed_at, 1);
        assert_eq!(retained.last_attempted_at, 2);
        assert!(retained.succeeded());
    }

    #[test]
    fn watch_countdown_uses_compact_time() {
        assert_eq!(interval_label(5), "5s");
        assert_eq!(interval_label(60), "1m 00s");
        assert_eq!(interval_label(125), "2m 05s");
        assert!(watch_status(5, false, 80).contains("q/Ctrl-C to exit"));
        assert_eq!(
            watch_status(5, false, 12),
            format!("{MUTED}5s · q{MUTED:#}")
        );
        assert!(watch_status(5, true, 20).contains("! 5s · q"));
    }
}
