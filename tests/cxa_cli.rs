use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};
use tempfile::TempDir;

struct Case {
    _root: TempDir,
    home: PathBuf,
    codex_home: PathBuf,
    codex: PathBuf,
    store: PathBuf,
}

struct PtyChild {
    child: Child,
    master: File,
    _slave: File,
    original_mode: libc::termios,
    output: Vec<u8>,
}

impl PtyChild {
    fn spawn(mut command: Command) -> Self {
        let mut master_fd = -1;
        let mut slave_fd = -1;
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master_fd,
                    &mut slave_fd,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let master = unsafe { File::from_raw_fd(master_fd) };
        let slave = unsafe { File::from_raw_fd(slave_fd) };
        let mut original_mode = unsafe { std::mem::zeroed::<libc::termios>() };
        assert_eq!(unsafe { libc::tcgetattr(slave_fd, &mut original_mode) }, 0);

        command
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()));
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(slave_fd, libc::TIOCSCTTY as libc::c_ulong, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        Self {
            child,
            master,
            _slave: slave,
            original_mode,
            output: Vec::new(),
        }
    }

    fn wait_for_output(&mut self, expected: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            self.read_available(Duration::from_millis(100));
            if self
                .output
                .windows(expected.len())
                .any(|window| window == expected)
            {
                return;
            }
        }
        panic!(
            "PTY output never contained {:?}: {}",
            String::from_utf8_lossy(expected),
            String::from_utf8_lossy(&self.output)
        );
    }

    fn send(&mut self, input: &[u8]) {
        self.master.write_all(input).unwrap();
        self.master.flush().unwrap();
    }

    fn signal(&self, signal: libc::c_int) {
        assert_eq!(
            unsafe { libc::kill(self.child.id() as libc::pid_t, signal) },
            0
        );
    }

    fn wait_success(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                self.read_available(Duration::from_millis(100));
                assert!(
                    status.success(),
                    "PTY child failed with {status}: {}",
                    String::from_utf8_lossy(&self.output)
                );
                return;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!(
                    "PTY child did not exit: {}",
                    String::from_utf8_lossy(&self.output)
                );
            }
            self.read_available(Duration::from_millis(50));
        }
    }

    fn assert_terminal_restored(&self) {
        let mut current = unsafe { std::mem::zeroed::<libc::termios>() };
        assert_eq!(
            unsafe { libc::tcgetattr(self.master.as_raw_fd(), &mut current) },
            0
        );
        let interactive_flags = libc::ICANON | libc::ECHO;
        assert_eq!(
            current.c_lflag & interactive_flags,
            self.original_mode.c_lflag & interactive_flags
        );
        assert!(
            self.output
                .windows(b"\x1b[?25h".len())
                .any(|window| window == b"\x1b[?25h"),
            "cursor-show sequence missing from {}",
            String::from_utf8_lossy(&self.output)
        );
    }

    fn read_available(&mut self, timeout: Duration) {
        let mut descriptor = libc::pollfd {
            fd: self.master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout_ms = timeout.as_millis().min(libc::c_int::MAX as u128) as libc::c_int;
        let ready = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if ready <= 0 || descriptor.revents & libc::POLLIN == 0 {
            return;
        }
        let mut bytes = [0_u8; 4096];
        let read = unsafe {
            libc::read(
                self.master.as_raw_fd(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if read > 0 {
            self.output.extend_from_slice(&bytes[..read as usize]);
        }
    }
}

impl Drop for PtyChild {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl Case {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let codex_home = home.join(".codex");
        let codex = home.join("fake-codex");
        let store = home.join(".codex-auth");
        fs::create_dir_all(&codex_home).unwrap();
        write_executable(
            &codex,
            r#"#!/bin/sh
case "$*" in
*app-server*)
  mode=${FAKE_CREDENTIAL_STORE:-file}
  while IFS= read -r line; do
    case "$line" in
      *'"id":0'*) printf '%s\n' '{"id":0,"result":{}}' ;;
      *'"method":"config/read"'*)
        printf '{"id":1,"result":{"config":{"cli_auth_credentials_store":"%s"}}}\n' "$mode"
        ;;
    esac
  done
  exit 0
  ;;
esac
if [ "$1" = login ] && [ -n "$FAKE_AUTH" ]; then
  if [ -n "$FAKE_LOGIN_CONFIG" ]; then
    cp "$CODEX_HOME/config.toml" "$FAKE_LOGIN_CONFIG"
  fi
  if [ -n "$FAKE_LOGIN_ARGS" ]; then
    printf '%s\n' "$@" > "$FAKE_LOGIN_ARGS"
  fi
  cp "$FAKE_AUTH" "$CODEX_HOME/auth.json"
  exit 0
fi
exit 1
"#,
        );
        Self {
            _root: root,
            home,
            codex_home,
            codex,
            store,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cxa"));
        command
            .env("HOME", &self.home)
            .env("CODEX_HOME", &self.codex_home)
            .env("CXA_CODEX_BIN", &self.codex)
            .env("CXA_ACCOUNT_STORE", &self.store)
            .env("CXA_SKIP_USAGE_REFRESH", "1")
            .env_remove("CODEX_ACCESS_TOKEN")
            .env_remove("CODEX_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("NO_COLOR");
        command
    }

    fn run(&self, arguments: &[&str]) -> Output {
        self.command().args(arguments).output().unwrap()
    }

    fn seed(&self, email: &str, user: &str, account: &str) {
        write_auth(
            &self.codex_home.join("auth.json"),
            email,
            user,
            account,
            "token-one",
        );
        let output = self.run(&["init", "--yes"]);
        assert_success(&output);
    }

    fn add_api(&self, name: &str, base_url: &str, key: &str) -> Output {
        self.add_api_with_options(name, base_url, key, &["--api-type", "cpa"])
    }

    fn add_api_with_options(
        &self,
        name: &str,
        base_url: &str,
        key: &str,
        options: &[&str],
    ) -> Output {
        let mut child = self
            .command()
            .args([
                "add",
                "--type",
                "api",
                "--name",
                name,
                "--base-url",
                base_url,
                "--api-key-stdin",
            ])
            .args(options)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(child.stdin.take().unwrap(), "{key}").unwrap();
        child.wait_with_output().unwrap()
    }
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_auth(path: &Path, email: &str, user_id: &str, account_id: &str, access_token: &str) {
    write_auth_at(
        path,
        email,
        user_id,
        account_id,
        access_token,
        "2026-08-28T00:00:00Z",
    );
}

fn write_auth_at(
    path: &Path,
    email: &str,
    user_id: &str,
    account_id: &str,
    access_token: &str,
    last_refresh: &str,
) {
    let claims = json!({
        "email": email,
        "https://api.openai.com/auth": {"chatgpt_user_id": user_id}
    });
    let id_token = format!(
        "header.{}.signature",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
    );
    let value = json!({
        "last_refresh": last_refresh,
        "tokens": {
            "id_token": id_token,
            "access_token": access_token,
            "refresh_token": format!("refresh-{access_token}"),
            "account_id": account_id
        }
    });
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

fn access_token(path: &Path) -> String {
    let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    value["tokens"]["access_token"].as_str().unwrap().to_owned()
}

fn write_executable(path: &Path, contents: &str) {
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true).mode(0o755);
    use std::io::Write as _;
    options
        .open(path)
        .unwrap()
        .write_all(contents.as_bytes())
        .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn sleeping_codex(case: &Case) -> (PathBuf, Child) {
    let path = case.home.join("codex-running");
    write_executable(&path, "#!/bin/sh\nsleep 30\n");
    let child = Command::new(&path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    (path, child)
}

#[test]
fn bare_command_recommends_init_for_the_current_login() {
    let case = Case::new();
    write_auth(
        &case.codex_home.join("auth.json"),
        "current@example.com",
        "user-current",
        "account-current",
        "current",
    );

    let output = case.run(&[]);

    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Found the current Codex login: current@example.com"));
    assert!(stdout.contains("cxa is not initialized. Run: cxa init"));
}

#[test]
fn redirected_init_requires_yes_without_creating_a_profile() {
    let case = Case::new();
    write_auth(
        &case.codex_home.join("auth.json"),
        "current@example.com",
        "user-current",
        "account-current",
        "current",
    );

    let output = case.run(&["init"]);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cxa init --yes"));
    assert!(!case.store.join("profile-1/auth.json").exists());
}

#[test]
fn init_imports_and_selects_the_current_login() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");

    assert_eq!(
        access_token(&case.store.join("profile-1/auth.json")),
        "token-one"
    );
}

#[test]
fn switch_works_while_codex_is_running_and_prints_restart_guidance() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");
    let imported = case.home.join("two.json");
    write_auth(
        &imported,
        "two@example.com",
        "user-two",
        "account-two",
        "token-two",
    );
    assert_success(&case.run(&["import", imported.to_str().unwrap()]));
    let (_codex, mut child) = sleeping_codex(&case);

    let output = case.run(&["use", "two"]);
    let _ = child.kill();
    let _ = child.wait();

    assert_success(&output);
    assert_eq!(
        access_token(&case.codex_home.join("auth.json")),
        "token-two"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(
        "Restart Codex or ChatGPT before expecting an existing session to use this account."
    ));
}

#[test]
fn switching_preserves_live_credentials_when_timestamps_tie() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");
    let second = case.home.join("second.json");
    write_auth(&second, "two@example.com", "user-two", "account-two", "two");
    assert_success(&case.run(&["import", second.to_str().unwrap()]));
    write_auth_at(
        &case.codex_home.join("auth.json"),
        "one@example.com",
        "user-one",
        "account-one",
        "refreshed-one",
        "2026-08-28T00:00:00Z",
    );

    assert_success(&case.run(&["two"]));
    assert_success(&case.run(&["one"]));

    assert_eq!(
        access_token(&case.codex_home.join("auth.json")),
        "refreshed-one"
    );
}

#[test]
fn switching_replaces_the_session_symlink_without_touching_its_old_target() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");
    let imported = case.home.join("two.json");
    write_auth(
        &imported,
        "two@example.com",
        "user-two",
        "account-two",
        "token-two",
    );
    assert_success(&case.run(&["import", imported.to_str().unwrap()]));
    let old_target = case.home.join("old-active.json");
    fs::rename(case.codex_home.join("auth.json"), &old_target).unwrap();
    symlink(&old_target, case.codex_home.join("auth.json")).unwrap();

    assert_success(&case.run(&["two"]));

    assert_eq!(
        access_token(&case.codex_home.join("auth.json")),
        "token-two"
    );
    assert_eq!(access_token(&old_target), "token-one");
    assert!(
        !fs::symlink_metadata(case.codex_home.join("auth.json"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn import_rejects_duplicate_account_identity() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");
    let duplicate = case.home.join("duplicate.json");
    write_auth(
        &duplicate,
        "renamed@example.com",
        "user-one",
        "account-one",
        "new-token",
    );

    let output = case.run(&["import", duplicate.to_str().unwrap()]);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("already enrolled as account one"));
}

#[test]
fn same_email_in_different_workspaces_remains_distinct() {
    let case = Case::new();
    case.seed("same@example.com", "same-user", "workspace-one");
    let second = case.home.join("second.json");
    write_auth(
        &second,
        "same@example.com",
        "same-user",
        "workspace-two",
        "second",
    );

    assert_success(&case.run(&["import", second.to_str().unwrap()]));

    assert!(case.store.join("profile-2/auth.json").is_file());
}

#[test]
fn add_runs_login_in_an_isolated_home() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");
    let fresh = case.home.join("fresh.json");
    write_auth(
        &fresh,
        "two@example.com",
        "user-two",
        "account-two",
        "token-two",
    );
    let login_args = case.home.join("login-args.txt");
    let output = case
        .command()
        .env("FAKE_AUTH", &fresh)
        .env("FAKE_LOGIN_ARGS", &login_args)
        .args(["add", "--type", "chatgpt"])
        .output()
        .unwrap();

    assert_success(&output);
    assert_eq!(
        access_token(&case.store.join("profile-2/auth.json")),
        "token-two"
    );
    assert_eq!(
        access_token(&case.codex_home.join("auth.json")),
        "token-one"
    );
    assert!(
        !fs::read_to_string(login_args)
            .unwrap()
            .lines()
            .any(|argument| argument == "--device-auth")
    );
}

#[test]
fn remove_saved_oauth_and_api_accounts_preserves_live_login_config_and_sessions() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");
    let fresh = case.home.join("fresh.json");
    write_auth(
        &fresh,
        "two@example.com",
        "user-two",
        "account-two",
        "token-two",
    );
    assert_success(&case.run(&["import", fresh.to_str().unwrap()]));
    assert_success(&case.add_api("gateway", "https://example.com/v1", "dummy-key"));
    fs::write(case.store.join("profile-2/usage.json"), "{}").unwrap();
    fs::write(case.codex_home.join("config.toml"), PERSONAL_CONFIG).unwrap();
    let sessions = case.codex_home.join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(sessions.join("rollout.jsonl"), "keep").unwrap();
    let auth = fs::read(case.codex_home.join("auth.json")).unwrap();
    assert_success(&case.run(&["remove", "two", "--yes"]));
    assert_success(&case.run(&["remove", "gateway", "--yes"]));
    assert!(!case.store.join("profile-2").exists());
    assert!(!case.store.join("profile-3").exists());
    assert!(case.store.join("profile-1/auth.json").exists());
    assert_eq!(fs::read(case.codex_home.join("auth.json")).unwrap(), auth);
    assert_eq!(
        fs::read_to_string(case.codex_home.join("config.toml")).unwrap(),
        PERSONAL_CONFIG
    );
    assert_eq!(
        fs::read_to_string(sessions.join("rollout.jsonl")).unwrap(),
        "keep"
    );
}

#[test]
fn remove_allows_inactive_oauth_in_api_mode_and_preserves_switch_back() {
    let case = Case::new();
    case.seed("personal@example.com", "personal", "personal");
    fs::write(case.codex_home.join("config.toml"), PERSONAL_CONFIG).unwrap();
    assert!(!case.run(&["remove", "personal", "--yes"]).status.success());
    let other = case.home.join("other.json");
    write_auth(&other, "other@example.com", "other", "other", "other-token");
    assert_success(&case.run(&["import", other.to_str().unwrap()]));
    let (url, server) = api_server(&[("200 OK", MODEL_LIST)]);
    assert_success(&case.add_api("company", &url, "dummy-key"));
    assert_success(&case.run(&["use", "company"]));
    server.join().unwrap();
    let config = fs::read(case.codex_home.join("config.toml")).unwrap();
    let auth = fs::read(case.codex_home.join("auth.json")).unwrap();
    let output = case.run(&["remove", "company", "--yes"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("still referenced"));
    assert_success(&case.run(&["remove", "personal", "--yes"]));
    assert!(!case.store.join("profile-1").exists());
    assert!(case.store.join("profile-3/api.json").exists());
    assert_eq!(
        fs::read(case.codex_home.join("config.toml")).unwrap(),
        config
    );
    assert_eq!(fs::read(case.codex_home.join("auth.json")).unwrap(), auth);
    assert_success(&case.run(&["use", "other"]));
    assert_eq!(
        access_token(&case.codex_home.join("auth.json")),
        "other-token"
    );
    assert_eq!(
        read_config(&case),
        toml_edit::de::from_str::<Value>(PERSONAL_CONFIG).unwrap()
    );
}

#[test]
fn remove_keeps_an_oauth_profile_when_session_auth_is_a_symlink_into_it() {
    let case = Case::new();
    case.seed("personal@example.com", "personal", "personal");
    fs::write(case.codex_home.join("config.toml"), PERSONAL_CONFIG).unwrap();
    fs::remove_file(case.codex_home.join("auth.json")).unwrap();
    std::os::unix::fs::symlink(
        case.store.join("profile-1/auth.json"),
        case.codex_home.join("auth.json"),
    )
    .unwrap();
    let (url, server) = api_server(&[("200 OK", MODEL_LIST)]);
    assert_success(&case.add_api("company", &url, "dummy-key"));
    assert_success(&case.run(&["use", "company"]));
    server.join().unwrap();
    assert!(!case.run(&["remove", "personal", "--yes"]).status.success());
    assert!(case.codex_home.join("auth.json").exists());
}

#[test]
fn remove_requires_confirmation_and_cancel_keeps_the_profile() {
    let case = Case::new();
    assert_success(&case.add_api("gateway", "https://example.com/v1", "dummy-key"));
    assert!(!case.run(&["remove", "gateway"]).status.success());
    for answer in [b"n\n".as_slice(), b"y\n".as_slice()] {
        let mut command = case.command();
        command.args(["remove", "gateway"]);
        let mut removal = PtyChild::spawn(command);
        removal.wait_for_output(b"Remove this account? [y/N]:");
        removal.send(answer);
        removal.wait_success();
        assert_eq!(case.store.join("profile-1").exists(), answer == b"n\n");
    }
}

#[test]
fn remove_rejects_profile_directory_symlinks() {
    let case = Case::new();
    let outside = case.home.join("outside-profile");
    fs::create_dir_all(&outside).unwrap();
    write_auth(
        &outside.join("auth.json"),
        "other@example.com",
        "other",
        "other",
        "dummy-token",
    );
    fs::create_dir_all(&case.store).unwrap();
    std::os::unix::fs::symlink(&outside, case.store.join("profile-1")).unwrap();
    assert!(!case.run(&["remove", "other", "--yes"]).status.success());
    assert!(outside.join("auth.json").exists());
}

#[test]
fn add_forwards_device_auth_to_codex_login() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");
    let fresh = case.home.join("fresh.json");
    write_auth(
        &fresh,
        "two@example.com",
        "user-two",
        "account-two",
        "token-two",
    );
    let login_args = case.home.join("login-args.txt");
    let output = case
        .command()
        .env("FAKE_AUTH", &fresh)
        .env("FAKE_LOGIN_ARGS", &login_args)
        .args(["add", "--device-auth"])
        .output()
        .unwrap();

    assert_success(&output);
    assert!(
        fs::read_to_string(login_args)
            .unwrap()
            .lines()
            .any(|argument| argument == "--device-auth")
    );
}

#[test]
fn relogin_rejects_a_different_account() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");
    let wrong = case.home.join("wrong.json");
    write_auth(
        &wrong,
        "wrong@example.com",
        "wrong-user",
        "wrong-account",
        "wrong",
    );
    let output = case
        .command()
        .env("FAKE_AUTH", &wrong)
        .args(["relogin", "one"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert_eq!(
        access_token(&case.store.join("profile-1/auth.json")),
        "token-one"
    );
}

#[test]
fn selected_relogin_updates_the_session_and_prints_restart_guidance() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");
    let replacement = case.home.join("replacement.json");
    write_auth(
        &replacement,
        "one@example.com",
        "user-one",
        "account-one",
        "replacement",
    );
    let output = case
        .command()
        .env("FAKE_AUTH", &replacement)
        .args(["relogin", "one"])
        .output()
        .unwrap();

    assert_success(&output);
    assert_eq!(
        access_token(&case.codex_home.join("auth.json")),
        "replacement"
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Restart Codex or ChatGPT"));
}

#[test]
fn list_preserves_rotation_and_restart_guidance_when_quota_fails() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");
    let refreshed = case.home.join("refreshed.json");
    write_auth_at(
        &refreshed,
        "one@example.com",
        "user-one",
        "account-one",
        "refreshed-one",
        "2026-08-28T00:00:00Z",
    );
    let codex = case.home.join("fake-codex");
    write_executable(
        &codex,
        r#"#!/bin/sh
case "$CODEX_HOME" in
  "$CXA_ACCOUNT_STORE"/.quota-*) ;;
  *)
    while IFS= read -r line; do
      case "$line" in
        *'"id":0'*) printf '%s\n' '{"id":0,"result":{}}' ;;
        *'"method":"config/read"'*) printf '%s\n' '{"id":1,"result":{"config":{"cli_auth_credentials_store":"file"}}}' ;;
      esac
    done
    exit 0
    ;;
esac
while IFS= read -r line; do
  case "$line" in
    *'"id":0'*) printf '%s\n' '{"id":0,"result":{}}' ;;
    *'"id":1'*)
      case "$line" in *'"refreshToken":false'*) ;; *) exit 2 ;; esac
      printf '%s\n' '{"id":1,"result":{}}'
      ;;
    *'"id":2'*)
      cp "$FAKE_REFRESHED" "$CODEX_HOME/auth.json"
      printf '%s\n' '{"id":2,"error":{"code":-32000,"message":"quota failed"}}'
      ;;
  esac
done
"#,
    );

    let output = case
        .command()
        .env_remove("CXA_SKIP_USAGE_REFRESH")
        .env("CXA_CODEX_BIN", &codex)
        .env("FAKE_REFRESHED", &refreshed)
        .arg("list")
        .output()
        .unwrap();

    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("quota unavailable (Protocol)"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("Restart Codex or ChatGPT"));
    assert_eq!(
        access_token(&case.store.join("profile-1/auth.json")),
        "refreshed-one"
    );
    assert_eq!(
        access_token(&case.codex_home.join("auth.json")),
        "refreshed-one"
    );
}

#[test]
fn list_attributes_quota_to_each_saved_profile() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");
    let second = case.home.join("second.json");
    write_auth(
        &second,
        "two@example.com",
        "user-two",
        "account-two",
        "token-two",
    );
    assert_success(&case.run(&["import", second.to_str().unwrap()]));
    let codex = case.home.join("fake-codex");
    write_executable(
        &codex,
        r#"#!/bin/sh
case "$CODEX_HOME" in
  "$CXA_ACCOUNT_STORE"/.quota-*) ;;
  *)
    while IFS= read -r line; do
      case "$line" in
        *'"id":0'*) printf '%s\n' '{"id":0,"result":{}}' ;;
        *'"method":"config/read"'*) printf '%s\n' '{"id":1,"result":{"config":{"cli_auth_credentials_store":"file"}}}' ;;
      esac
    done
    exit 0
    ;;
esac
if grep -q token-one "$CODEX_HOME/auth.json"; then
  used=11
  spark=0
  account=one
else
  used=77
  spark=100
  account=two
fi
touch "$CXA_ACCOUNT_STORE/$account.started"
attempt=0
while [ ! -e "$CXA_ACCOUNT_STORE/one.started" ] || [ ! -e "$CXA_ACCOUNT_STORE/two.started" ]; do
  attempt=$((attempt + 1))
  [ "$attempt" -lt 100 ] || exit 9
  sleep 0.01
done
if [ "$account" = one ]; then sleep 0.2; fi
while IFS= read -r line; do
  case "$line" in
    *'"id":0'*) printf '%s\n' '{"id":0,"result":{}}' ;;
    *'"id":1'*) printf '%s\n' '{"id":1,"result":{}}' ;;
    *'"id":2'*)
      printf '{"id":2,"result":{"rateLimitsByLimitId":{"codex":{"limitId":"codex","planType":"pro","primary":{"usedPercent":%s,"windowDurationMins":10080}},"codex_bengalfox":{"limitId":"codex_bengalfox","limitName":"GPT-5.3-Codex-Spark","planType":"pro","primary":{"usedPercent":0,"windowDurationMins":300},"secondary":{"usedPercent":%s,"windowDurationMins":10080}}}}}\n' "$used" "$spark"
      ;;
  esac
done
"#,
    );

    let output = case
        .command()
        .env_remove("CXA_SKIP_USAGE_REFRESH")
        .env("CXA_CODEX_BIN", &codex)
        .arg("list")
        .output()
        .unwrap();

    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let account_two = stdout.find("2. two ·").unwrap();
    let account_one_output = &stdout[..account_two];
    let account_two_output = &stdout[account_two..];
    assert!(account_one_output.contains("1. one · Pro 20x"));
    assert!(account_one_output.contains("one@example.com · updated just now"));
    assert!(account_one_output.contains("89% left"));
    assert!(!account_one_output.contains("23% left"));
    assert!(account_two_output.contains("23% left"));
    assert!(account_two_output.contains("Codex Spark  EXHAUSTED"));
    assert!(account_two_output.contains("[░░░░░░░░░░░░░░░░]    0% left"));
    assert!(!stdout.contains("codex primary"));
    assert!(stdout.lines().all(|line| line.chars().count() <= 80));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("Fetching usage"));
}

#[test]
fn status_infers_selection_when_codex_changes_to_an_enrolled_account() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");
    let second = case.home.join("second.json");
    write_auth(&second, "two@example.com", "user-two", "account-two", "two");
    assert_success(&case.run(&["import", second.to_str().unwrap()]));
    fs::copy(&second, case.codex_home.join("auth.json")).unwrap();

    let output = case.run(&["status"]);

    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("* two · ChatGPT\n  two@example.com"));
}

#[test]
fn relative_configuration_paths_are_rejected() {
    let case = Case::new();
    let output = case
        .command()
        .env("CXA_ACCOUNT_STORE", "relative-store")
        .arg("status")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("must be an absolute path"));
}

#[test]
fn overlapping_codex_home_and_account_store_are_rejected() {
    let case = Case::new();
    let output = case
        .command()
        .env("CXA_ACCOUNT_STORE", case.codex_home.join("accounts"))
        .arg("status")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("must be separate directories"));
}

#[test]
fn redirected_output_contains_no_colour_codes() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");

    let output = case.run(&["status"]);

    assert_success(&output);
    assert!(!output.stdout.windows(2).any(|bytes| bytes == b"\x1b["));

    let empty = Case::new();
    let output = empty.run(&["list"]);
    assert_success(&output);
    assert!(!output.stdout.windows(2).any(|bytes| bytes == b"\x1b["));
}

#[test]
fn watch_requires_an_interactive_terminal() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");

    for arguments in [["watch"].as_slice(), ["list", "--watch"].as_slice()] {
        let output = case.run(arguments);

        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("Watch mode requires an interactive terminal")
        );
    }
}

#[test]
fn watch_exit_remains_responsive_while_the_account_lock_is_held() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");
    let _lock = cxa::fs::ExclusiveLock::acquire(&case.store.join("switch.lock")).unwrap();
    let mut command = case.command();
    command.arg("watch");
    let mut watch = PtyChild::spawn(command);

    watch.wait_for_output(b"\x1b[?25l");
    watch.send(b"q");
    watch.wait_success();
    watch.assert_terminal_restored();
}

#[test]
fn unrelated_keys_do_not_consume_the_watch_interval() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");
    let mut command = case.command();
    command.args(["watch", "--interval", "5"]);
    let mut watch = PtyChild::spawn(command);

    watch.wait_for_output(b"refresh in 5s");
    for _ in 0..10 {
        watch.send(b"\x1b[A");
    }
    thread::sleep(Duration::from_millis(250));
    watch.read_available(Duration::from_millis(50));
    assert!(
        !watch
            .output
            .windows(b"refresh in 4s".len())
            .any(|window| window == b"refresh in 4s"),
        "unrelated input advanced the countdown: {}",
        String::from_utf8_lossy(&watch.output)
    );
    watch.send(b"q");
    watch.wait_success();
    watch.assert_terminal_restored();
}

#[test]
fn watch_cancels_active_quota_workers_before_exiting() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");
    let codex = case.home.join("slow-codex");
    write_executable(
        &codex,
        r#"#!/bin/sh
case "$CODEX_HOME" in
  "$CXA_ACCOUNT_STORE"/.quota-*)
    trap 'touch "$CXA_ACCOUNT_STORE/quota.stopped"; exit 0' HUP INT TERM
    while IFS= read -r line; do
      case "$line" in
        *'"id":0'*) printf '%s\n' '{"id":0,"result":{}}' ;;
        *'"id":1'*) printf '%s\n' '{"id":1,"result":{}}' ;;
        *'"id":2'*)
          touch "$CXA_ACCOUNT_STORE/quota.started"
          while :; do sleep 1; done
          ;;
      esac
    done
    ;;
  *)
    while IFS= read -r line; do
      case "$line" in
        *'"id":0'*) printf '%s\n' '{"id":0,"result":{}}' ;;
        *'"method":"config/read"'*) printf '%s\n' '{"id":1,"result":{"config":{"cli_auth_credentials_store":"file"}}}' ;;
      esac
    done
    ;;
esac
"#,
    );
    let mut command = case.command();
    command
        .env_remove("CXA_SKIP_USAGE_REFRESH")
        .env("CXA_CODEX_BIN", &codex)
        .arg("watch");
    let mut watch = PtyChild::spawn(command);

    watch.wait_for_output(b"loading");
    let started = case.store.join("quota.started");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !started.is_file() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(started.is_file());
    watch.send(b"q");
    watch.wait_success();
    watch.assert_terminal_restored();
    let reserved = watch
        .output
        .windows(b"\x1b[3A\x1b[s".len())
        .position(|window| window == b"\x1b[3A\x1b[s")
        .unwrap_or_else(|| panic!("no reserved origin in {:?}", watch.output));
    let loading = watch
        .output
        .windows(b"loading".len())
        .position(|window| window == b"loading")
        .unwrap();
    assert!(reserved < loading);
    assert!(
        watch
            .output
            .windows(b"\x1b[s".len())
            .any(|window| window == b"\x1b[s")
    );
    assert!(
        watch
            .output
            .windows(b"\x1b[u\x1b[J".len())
            .any(|window| window == b"\x1b[u\x1b[J")
    );
    assert!(case.store.join("quota.stopped").is_file());
    assert!(fs::read_dir(&case.store).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".quota-")
    }));
}

#[test]
fn termination_signal_restores_watch_terminal_state() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");
    let mut command = case.command();
    command.arg("watch");
    let mut watch = PtyChild::spawn(command);

    watch.wait_for_output(b"Watching");
    watch.signal(libc::SIGTERM);
    watch.wait_success();
    watch.assert_terminal_restored();
}

#[test]
fn informational_flags_do_not_require_home_configuration() {
    let output = Command::new(env!("CARGO_BIN_EXE_cxa"))
        .env_remove("HOME")
        .arg("--version")
        .output()
        .unwrap();

    assert_success(&output);
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .starts_with(&format!("cxa {}", env!("CARGO_PKG_VERSION")))
    );
}

#[test]
fn absolute_path_overrides_do_not_require_home() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");

    let output = case
        .command()
        .env_remove("HOME")
        .arg("status")
        .output()
        .unwrap();

    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("one@example.com"));
}

#[test]
fn non_file_codex_credentials_are_rejected_before_switching() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");
    let output = case
        .command()
        .env("FAKE_CREDENTIAL_STORE", "keyring")
        .arg("1")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("cxa requires Codex's file credential store")
    );
}

#[test]
fn malformed_enrolled_profile_is_reported() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");
    let profile = case.store.join("profile-2");
    fs::create_dir_all(&profile).unwrap();
    fs::write(profile.join("auth.json"), b"not json").unwrap();

    let output = case.run(&["list"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("profile-2/auth.json"));
    assert!(stderr.contains("invalid JSON"));
}

#[test]
fn credential_environment_overrides_are_rejected_before_switching() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");

    let output = case
        .command()
        .env("CODEX_ACCESS_TOKEN", "external-token")
        .arg("1")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Unset CODEX_ACCESS_TOKEN"));
}

#[test]
fn api_key_environment_does_not_block_file_credentials() {
    let case = Case::new();
    case.seed("one@example.com", "user-one", "account-one");

    let output = case
        .command()
        .env("OPENAI_API_KEY", "unrelated")
        .env("CODEX_API_KEY", "unrelated")
        .arg("status")
        .output()
        .unwrap();

    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("one@example.com"));
}

const PERSONAL_CONFIG: &str = r#"# Personal defaults must survive switching.
model = "gpt-6.1-sol"
model_provider = "unicodex"
model_reasoning_effort = "high"
approval_policy = "on-request"

[model_providers.unicodex]
name = "OpenAI"
wire_api = "responses"
requires_openai_auth = true
stream_idle_timeout_ms = 600000 # Keep provider tuning

[mcp_servers.example]
command = "example-mcp"
args = ["--read-only"]

[projects."/workspace"]
trust_level = "trusted"
"#;

fn read_config(case: &Case) -> Value {
    toml_edit::de::from_str(&fs::read_to_string(case.codex_home.join("config.toml")).unwrap())
        .unwrap()
}

#[test]
fn oauth_switch_from_an_unmanaged_relay_builds_official_connection_without_changing_provider() {
    let case = Case::new();
    case.seed("personal@example.com", "personal", "personal");
    let mut document = PERSONAL_CONFIG.parse::<toml_edit::DocumentMut>().unwrap();
    let table = document["model_providers"]["unicodex"]
        .as_table_like_mut()
        .unwrap();
    for (key, text) in [
        ("name", "copilot"),
        ("base_url", "https://old-relay.example/v1"),
        ("env_key", "OLD_RELAY_KEY"),
        ("experimental_bearer_token", "unused-key"),
    ] {
        table.insert(key, toml_edit::value(text));
    }
    let mut auth = toml_edit::Table::new();
    auth.insert("command", toml_edit::value("old-auth-command"));
    table.insert("auth", toml_edit::Item::Table(auth));
    let mut extra = toml_edit::Table::new();
    extra.insert("flag", toml_edit::value(true));
    table.insert("extra", toml_edit::Item::Table(extra));
    document["openai_base_url"] = toml_edit::value("https://stale-built-in.example/v1");
    fs::write(case.codex_home.join("config.toml"), document.to_string()).unwrap();
    let live_auth = fs::read(case.codex_home.join("auth.json")).unwrap();
    assert_success(&case.run(&["use", "personal"]));
    let current = read_config(&case);
    assert_eq!(current["model_provider"], "unicodex");
    let table = &current["model_providers"]["unicodex"];
    assert_eq!(table["name"], "OpenAI");
    assert_eq!(table["wire_api"], "responses");
    assert_eq!(table["requires_openai_auth"], true);
    for key in ["base_url", "env_key", "experimental_bearer_token", "auth"] {
        assert!(table.get(key).is_none());
    }
    assert!(current.get("openai_base_url").is_none());
    assert_eq!(table["stream_idle_timeout_ms"], 600000);
    assert_eq!(table["extra"]["flag"], true);
    for key in [
        "model",
        "model_reasoning_effort",
        "approval_policy",
        "mcp_servers",
        "projects",
    ] {
        let original: Value = toml_edit::de::from_str(PERSONAL_CONFIG).unwrap();
        assert_eq!(current[key], original[key]);
    }
    assert_eq!(
        fs::read(case.codex_home.join("auth.json")).unwrap(),
        live_auth
    );
}

#[test]
fn builtin_oauth_switch_removes_a_stale_relay_url() {
    let case = Case::new();
    case.seed("personal@example.com", "personal", "personal");
    fs::write(case.codex_home.join("config.toml"), "model = \"gpt-6.1-sol\"\nmodel_reasoning_effort = \"high\"\nopenai_base_url = \"https://old-relay.example/v1\"\n").unwrap();
    assert_success(&case.run(&["use", "personal"]));
    let current = read_config(&case);
    assert!(current.get("model_provider").is_none());
    assert!(current.get("openai_base_url").is_none());
    assert_eq!(current["model_reasoning_effort"], "high");
}

#[test]
fn adding_chatgpt_uses_official_connection_without_rewriting_live_relay_config() {
    let case = Case::new();
    case.seed("personal@example.com", "personal", "personal");
    let relay = PERSONAL_CONFIG.replace("name = \"OpenAI\"", "name = \"old-relay\"\nbase_url = \"https://old-relay.example/v1\"\nenv_key = \"OLD_RELAY_KEY\"");
    fs::write(case.codex_home.join("config.toml"), &relay).unwrap();
    let auth_before = fs::read(case.codex_home.join("auth.json")).unwrap();
    let fresh = case.home.join("fresh.json");
    write_auth(&fresh, "other@example.com", "other", "other", "other-token");
    let captured = case.home.join("login-config.toml");
    let output = case
        .command()
        .env("FAKE_AUTH", &fresh)
        .env("FAKE_LOGIN_CONFIG", &captured)
        .args(["add", "--type", "chatgpt"])
        .output()
        .unwrap();
    assert_success(&output);
    let official: Value = toml_edit::de::from_str(&fs::read_to_string(captured).unwrap()).unwrap();
    assert_eq!(official["model_provider"], "unicodex");
    let table = &official["model_providers"]["unicodex"];
    assert_eq!(table["name"], "OpenAI");
    assert_eq!(table["requires_openai_auth"], true);
    assert!(table.get("base_url").is_none());
    assert!(table.get("env_key").is_none());
    assert_eq!(
        fs::read_to_string(case.codex_home.join("config.toml")).unwrap(),
        relay
    );
    assert_eq!(
        fs::read(case.codex_home.join("auth.json")).unwrap(),
        auth_before
    );
}

#[test]
fn root_help_explains_supported_accounts_and_restart_requirement() {
    let case = Case::new();
    let output = case.run(&["--help"]);
    assert_success(&output);
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("ChatGPT subscription accounts"));
    assert!(help.contains("Responses-compatible API-key accounts"));
    assert!(help.contains("model_provider"));
    assert!(help.contains("Existing Codex processes must be restarted"));
    assert!(help.contains("Generic API accounts do not query quota"));
    assert!(!case.store.exists());
}

#[test]
fn api_add_help_explains_api_types_and_local_only_enrollment() {
    let case = Case::new();
    let output = case.run(&["add", "--help"]);
    assert_success(&output);
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("interactive wizard"));
    assert!(help.contains("--type chatgpt or --type api"));
    assert!(help.contains("isolated home"));
    assert!(help.contains("CLIProxyAPI (CPA)"));
    assert!(help.contains("New API accounts default to generic"));
    assert!(help.contains("--api-type cpa"));
    assert!(help.contains("optional website"));
    assert!(help.contains("OpenAI Responses API and /models"));
    assert!(help.contains("/v0/resource/plugins/cpa-key-billing/subscription"));
    assert!(help.contains("using the enrolled key as a Bearer token"));
    assert!(help.contains("cxa does not install or load server plugins"));
    assert!(help.contains("does not verify the key"));
    assert!(help.contains("cxa use ACCOUNT"));
    assert!(help.contains("cxa list"));
    assert!(!case.store.exists());
}

#[test]
fn interactive_api_add_identifies_cpa_and_uses_visible_key_input() {
    let case = Case::new();
    case.seed("personal@example.com", "personal", "personal");
    fs::write(case.codex_home.join("config.toml"), PERSONAL_CONFIG).unwrap();
    let auth = fs::read(case.codex_home.join("auth.json")).unwrap();
    let mut command = case.command();
    command.args([
        "add",
        "--type",
        "api",
        "--api-type",
        "cpa",
        "--name",
        "company",
        "--base-url",
        "http://company.test:8317/v1",
    ]);
    let mut enrollment = PtyChild::spawn(command);
    enrollment.wait_for_output(b"API key:");
    assert!(!String::from_utf8_lossy(&enrollment.output).contains("(visible)"));
    enrollment.send(b"secret-interactive-key\n");
    enrollment.wait_for_output(b"Service website (optional, Enter to skip):");
    enrollment.send(b"\n");
    enrollment.wait_success();
    let output = String::from_utf8_lossy(&enrollment.output);
    assert!(output.contains("API type: CPA API key"));
    assert!(output.contains("GET + Bearer authentication"));
    assert!(output.contains("The API key has not been verified"));
    assert!(output.contains("secret-interactive-key"));
    assert_eq!(fs::read(case.codex_home.join("auth.json")).unwrap(), auth);
    assert_eq!(
        fs::read_to_string(case.codex_home.join("config.toml")).unwrap(),
        PERSONAL_CONFIG
    );
    assert!(!case.store.join("connection.json").exists());
}

#[test]
fn api_enrollment_preserves_login_and_config_and_hides_the_key() {
    let case = Case::new();
    case.seed("personal@example.com", "personal", "personal");
    fs::write(case.codex_home.join("config.toml"), PERSONAL_CONFIG).unwrap();
    let auth = fs::read(case.codex_home.join("auth.json")).unwrap();

    let output = case.add_api(
        "company",
        "http://company.test:8317/v1",
        "secret-company-key",
    );

    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("API type: CPA API key"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("GET + Bearer authentication"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("(CPA API key)"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("The API key has not been verified"));
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("current login and config.toml were not changed")
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("secret-company-key"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("secret-company-key"));
    assert_eq!(fs::read(case.codex_home.join("auth.json")).unwrap(), auth);
    assert_eq!(
        fs::read_to_string(case.codex_home.join("config.toml")).unwrap(),
        PERSONAL_CONFIG
    );
    assert_eq!(
        fs::metadata(case.store.join("profile-2/api.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(!case.store.join("connection.json").exists());
}

#[test]
fn custom_provider_switch_round_trip_preserves_general_settings_and_rotated_oauth() {
    let case = Case::new();
    case.seed("personal@example.com", "personal", "personal");
    fs::write(case.codex_home.join("config.toml"), PERSONAL_CONFIG).unwrap();
    let (base_url, server) = api_server(&[("200 OK", MODEL_LIST)]);
    assert_success(&case.add_api("company", &base_url, "company-key"));
    write_auth(
        &case.codex_home.join("auth.json"),
        "personal@example.com",
        "personal",
        "personal",
        "rotated-token",
    );
    let auth = fs::read(case.codex_home.join("auth.json")).unwrap();

    let output = case.run(&["use", "company"]);
    assert_success(&output);
    server.join().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Gateway models: gpt-6-sol, gpt-6.1-sol"));
    assert!(stdout.contains("Select an advertised model in that chat"));
    assert_eq!(read_config(&case)["model"], "gpt-6.1-sol");
    assert_eq!(fs::read(case.codex_home.join("auth.json")).unwrap(), auth);
    assert_eq!(read_config(&case)["model_provider"], "unicodex");
    assert_eq!(
        read_config(&case)["model_providers"]["unicodex"]["name"],
        "company"
    );
    assert_eq!(
        read_config(&case)["model_providers"]["unicodex"]["experimental_bearer_token"],
        "company-key"
    );
    assert_eq!(
        read_config(&case)["model_providers"]["unicodex"]["requires_openai_auth"],
        false
    );
    let output = case.run(&["list"]);
    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("* 2. company · CPA API key"));

    // A setting edited during company mode must not be overwritten by the saved route.
    let path = case.codex_home.join("config.toml");
    let edited = fs::read_to_string(&path)
        .unwrap()
        .replace("600000", "900000")
        .replace("\"high\"", "\"medium\"");
    fs::write(&path, edited).unwrap();
    assert_success(&case.run(&["use", "personal"]));

    let restored = read_config(&case);
    let expected: Value = toml_edit::de::from_str(
        &PERSONAL_CONFIG
            .replace("600000", "900000")
            .replace("\"high\"", "\"medium\""),
    )
    .unwrap();
    assert_eq!(restored, expected);
    assert_eq!(fs::read(case.codex_home.join("auth.json")).unwrap(), auth);
    assert_eq!(
        access_token(&case.store.join("profile-1/auth.json")),
        "rotated-token"
    );
    let text = fs::read_to_string(path).unwrap();
    assert!(text.contains("# Personal defaults must survive switching."));
    assert!(text.contains("# Keep provider tuning"));
    assert!(text.contains("# Keep provider tuning"));
    assert!(!text.contains("company-key"));
    assert!(!case.store.join("switch-pending.json").exists());
}

#[test]
fn interactive_generic_enrollment_prompts_in_order_and_saves_website() {
    let case = Case::new();
    let mut command = case.command();
    command.arg("add");
    let mut enrollment = PtyChild::spawn(command);
    enrollment.wait_for_output(b"Account type [1/2]:");
    enrollment.send(b"2\n");
    enrollment.wait_for_output(b"API base URL:");
    enrollment.send(b"https://gateway.example.com/v1\n");
    enrollment.wait_for_output(b"API key:");
    enrollment.send(b"dummy-visible-key\n");
    enrollment.wait_for_output(b"Service website (optional, Enter to skip):");
    enrollment.send(b"https://gateway.example.com/dashboard?tab=usage\n");
    enrollment.wait_for_output(b"Service type [generic/cpa, Enter=generic]:");
    enrollment.send(b"\n");
    enrollment.wait_for_output(b"Account name [gateway.example.com]:");
    enrollment.send(b"\n");
    enrollment.wait_success();
    let api: Value =
        serde_json::from_slice(&fs::read(case.store.join("profile-1/api.json")).unwrap()).unwrap();
    assert_eq!(api["name"], "gateway.example.com");
    assert_eq!(api["api_type"], "generic");
    assert_eq!(api["api_key"], "dummy-visible-key");
    assert_eq!(
        api["website"],
        "https://gateway.example.com/dashboard?tab=usage"
    );
    let output = case
        .command()
        .env_remove("CXA_SKIP_USAGE_REFRESH")
        .arg("list")
        .output()
        .unwrap();
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Website  https://gateway.example.com/dashboard?tab=usage"));
    assert!(stdout.contains("Quota query disabled (generic API)."));
    assert!(!stdout.contains("dummy-visible-key"));
    assert!(!stdout.contains("Usage unknown"));
    assert!(!case.store.join("profile-1/usage.json").exists());
    let mut command = case.command();
    command
        .env_remove("CXA_SKIP_USAGE_REFRESH")
        .args(["watch", "--interval", "5"]);
    let mut watch = PtyChild::spawn(command);
    watch.wait_for_output(b"Website  https://gateway.example.com/dashboard?tab=usage");
    watch.send(b"q");
    watch.wait_success();
    assert!(!case.store.join("profile-1/usage.json").exists());
}

#[test]
fn guided_chatgpt_enrollment_selects_login_method_without_changing_live_credentials() {
    for method in ["browser", "device"] {
        let case = Case::new();
        case.seed("one@example.com", "user-one", "account-one");
        fs::write(case.codex_home.join("config.toml"), PERSONAL_CONFIG).unwrap();
        let auth_before = fs::read(case.codex_home.join("auth.json")).unwrap();
        let fresh = case.home.join("fresh.json");
        write_auth(
            &fresh,
            "two@example.com",
            "user-two",
            "account-two",
            "token-two",
        );
        let login_args = case.home.join("login-args.txt");
        let mut command = case.command();
        command
            .env("FAKE_AUTH", &fresh)
            .env("FAKE_LOGIN_ARGS", &login_args)
            .arg("add");
        let mut enrollment = PtyChild::spawn(command);
        enrollment.wait_for_output(b"Account type [1/2]:");
        enrollment.send(b"1\n");
        enrollment.wait_for_output(b"Login method [browser/device, Enter=browser]:");
        enrollment.send(format!("{method}\n").as_bytes());
        enrollment.wait_for_output(b"Account name [two]:");
        enrollment.send(if method == "browser" {
            b"work\n"
        } else {
            b"\n"
        });
        enrollment.wait_success();
        assert_eq!(
            fs::read_to_string(case.store.join("profile-2/name")).unwrap(),
            if method == "browser" { "work" } else { "two" }
        );
        assert_eq!(
            access_token(&case.store.join("profile-2/auth.json")),
            "token-two"
        );
        assert_eq!(
            fs::read(case.codex_home.join("auth.json")).unwrap(),
            auth_before
        );
        assert_eq!(
            fs::read_to_string(case.codex_home.join("config.toml")).unwrap(),
            PERSONAL_CONFIG
        );
        assert_eq!(
            fs::read_to_string(login_args)
                .unwrap()
                .lines()
                .any(|arg| arg == "--device-auth"),
            method == "device"
        );
    }
}

#[test]
fn enrollment_rejects_mixed_types_and_requires_a_type_without_a_terminal() {
    let case = Case::new();
    for args in [
        vec!["add"],
        vec!["add", "--type", "api", "--device-auth"],
        vec![
            "add",
            "--type",
            "chatgpt",
            "--base-url",
            "https://example.com/v1",
        ],
        vec!["add", "--type", "unknown"],
        vec!["add", "--api-key"],
    ] {
        let output = case.run(&args);
        assert!(!output.status.success(), "unexpected success: {args:?}");
        assert!(!case.store.join("profile-1").exists());
        assert!(!case.codex_home.join("auth.json").exists());
    }
}

#[test]
fn generic_gateway_switch_and_status_preserve_oauth_without_querying_quota() {
    let case = Case::new();
    case.seed("personal@example.com", "personal", "personal");
    fs::write(case.codex_home.join("config.toml"), PERSONAL_CONFIG).unwrap();
    let auth = fs::read(case.codex_home.join("auth.json")).unwrap();
    let (base_url, server) = api_server(&[("200 OK", MODEL_LIST), ("200 OK", MODEL_LIST)]);
    assert_success(&case.add_api_with_options(
        "gateway",
        &base_url,
        "dummy-gateway-key",
        &["--website", "https://gateway.example.com/usage"],
    ));
    assert_success(&case.run(&["models", "gateway"]));
    assert_success(&case.run(&["use", "gateway"]));
    let requests = server.join().unwrap();
    assert!(
        requests
            .iter()
            .all(|request| request.starts_with("GET /proxy/v1/models "))
    );
    assert_eq!(read_config(&case)["model_provider"], "unicodex");
    assert_eq!(fs::read(case.codex_home.join("auth.json")).unwrap(), auth);
    // Cached data from a previous account type must not be displayed for generic accounts.
    fs::write(
        case.store.join("profile-2/usage.json"),
        serde_json::to_vec(&json!({
            "observed_at": 1, "last_attempted_at": 1,
            "cpa": {"unlimited": true, "dimensions": []}
        }))
        .unwrap(),
    )
    .unwrap();
    for command in ["status", "list"] {
        let mut process = case.command();
        if command == "status" {
            process.env_remove("CXA_SKIP_USAGE_REFRESH");
        }
        let output = process.arg(command).output().unwrap();
        assert_success(&output);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("gateway · API key"));
        assert!(stdout.contains("Website  https://gateway.example.com/usage"));
        assert!(stdout.contains("Quota query disabled (generic API)."));
        assert!(!stdout.contains("CPA allocation"));
        assert!(!stdout.contains("CPA quota request failed"));
        assert!(!stdout.contains("dummy-gateway-key"));
    }
    assert_success(&case.run(&["use", "personal"]));
    let expected: Value = toml_edit::de::from_str(PERSONAL_CONFIG).unwrap();
    assert_eq!(read_config(&case), expected);
    assert_eq!(fs::read(case.codex_home.join("auth.json")).unwrap(), auth);
}

#[test]
fn api_enrollment_rejects_invalid_website_and_unknown_type_without_changing_login() {
    let case = Case::new();
    case.seed("personal@example.com", "personal", "personal");
    let auth = fs::read(case.codex_home.join("auth.json")).unwrap();
    for website in [
        "javascript:alert(1)",
        "https://user:secret@gateway.example.com",
    ] {
        let output = case.add_api_with_options(
            "gateway",
            "https://gateway.example.com/v1",
            "dummy-key",
            &["--website", website],
        );
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Website must use HTTP(S)"));
        assert!(!case.store.join("profile-2").exists());
    }
    let output = case.run(&["add", "--type", "api", "--api-type", "auto"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid value"));
    assert_eq!(fs::read(case.codex_home.join("auth.json")).unwrap(), auth);
}

#[test]
fn legacy_api_profiles_without_type_still_query_cpa_quota() {
    let case = Case::new();
    let (base_url, server) = api_server(&[("200 OK", r#"{"subscription":{"unlimited":true}}"#)]);
    assert_success(&case.add_api("company", &base_url, "dummy-key"));
    let path = case.store.join("profile-1/api.json");
    let mut api: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    api.as_object_mut().unwrap().remove("api_type");
    fs::write(path, serde_json::to_vec(&api).unwrap()).unwrap();
    let output = case
        .command()
        .env_remove("CXA_SKIP_USAGE_REFRESH")
        .arg("list")
        .output()
        .unwrap();
    assert_success(&output);
    let requests = server.join().unwrap();
    assert!(
        requests[0].starts_with("GET /proxy/v0/resource/plugins/cpa-key-billing/subscription ")
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("CPA allocation  Unlimited"));
}

#[test]
fn guided_enrollment_can_select_cpa_and_skip_the_website() {
    let case = Case::new();
    let (base_url, server) = api_server(&[("200 OK", r#"{"subscription":{"unlimited":true}}"#)]);
    let mut command = case.command();
    command.args(["add", "--type", "api"]);
    let mut enrollment = PtyChild::spawn(command);
    enrollment.wait_for_output(b"API base URL:");
    enrollment.send(format!("{base_url}\n").as_bytes());
    enrollment.wait_for_output(b"API key:");
    enrollment.send(b"dummy-cpa-key\n");
    enrollment.wait_for_output(b"Service website (optional, Enter to skip):");
    enrollment.send(b"\n");
    enrollment.wait_for_output(b"Service type [generic/cpa, Enter=generic]:");
    enrollment.send(b"cpa\n");
    enrollment.wait_for_output(b"Account name [127.0.0.1]:");
    enrollment.send(b"company\n");
    enrollment.wait_success();
    let api: Value =
        serde_json::from_slice(&fs::read(case.store.join("profile-1/api.json")).unwrap()).unwrap();
    assert_eq!(api["api_type"], "cpa");
    assert!(api.get("website").is_none());
    let output = case
        .command()
        .env_remove("CXA_SKIP_USAGE_REFRESH")
        .arg("list")
        .output()
        .unwrap();
    assert_success(&output);
    let requests = server.join().unwrap();
    assert!(
        requests[0].starts_with("GET /proxy/v0/resource/plugins/cpa-key-billing/subscription ")
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("CPA allocation  Unlimited"));
}

#[test]
fn redirected_api_enrollment_requires_base_url() {
    let case = Case::new();
    for options in [
        vec!["add", "--type", "api", "--api-key-stdin"],
        vec![
            "add",
            "--type",
            "api",
            "--api-key-stdin",
            "--name",
            "gateway",
        ],
    ] {
        let output = case.run(&options);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("Scripted API enrollment requires --base-url")
        );
        assert!(!case.store.join("profile-1").exists());
    }
}

#[test]
fn optional_names_share_defaults_and_collisions_across_account_types() {
    let case = Case::new();
    case.seed("personal@example.com", "personal", "personal");
    fs::write(case.codex_home.join("config.toml"), PERSONAL_CONFIG).unwrap();
    let auth = fs::read(case.codex_home.join("auth.json")).unwrap();
    let fresh = case.home.join("fresh.json");
    write_auth(
        &fresh,
        "gateway.example.com@example.com",
        "new-user",
        "new-account",
        "new-token",
    );
    let output = case
        .command()
        .env("FAKE_AUTH", &fresh)
        .args(["add", "--type", "chatgpt", "--name", "work"])
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(
        fs::read_to_string(case.store.join("profile-2/name")).unwrap(),
        "work"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("Added work  gateway.example.com@example.com")
    );

    for expected in ["gateway.example.com", "gateway.example.com-2"] {
        let mut child = case
            .command()
            .args([
                "add",
                "--type",
                "api",
                "--base-url",
                "https://gateway.example.com/v1",
                "--api-key-stdin",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(child.stdin.take().unwrap(), "dummy-key").unwrap();
        let output = child.wait_with_output().unwrap();
        assert_success(&output);
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains(&format!("Added {expected} (API key)"))
        );
    }
    write_auth(
        &fresh,
        "gateway.example.com@another.example",
        "other-user",
        "other-account",
        "other-token",
    );
    let output = case
        .command()
        .env("FAKE_AUTH", &fresh)
        .args(["add", "--type", "chatgpt"])
        .output()
        .unwrap();
    assert_success(&output);
    assert_eq!(
        fs::read_to_string(case.store.join("profile-5/name")).unwrap(),
        "gateway.example.com-3"
    );
    for name in ["WORK", "123", "bad name", ""] {
        let output = case
            .command()
            .env("FAKE_AUTH", &fresh)
            .args(["add", "--type", "chatgpt", "--name", name])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!case.store.join("profile-6").exists());
        let output = case.add_api(name, "https://gateway.example.com/v1", "dummy-key");
        assert!(!output.status.success());
        assert!(!case.store.join("profile-6").exists());
    }
    assert_eq!(fs::read(case.codex_home.join("auth.json")).unwrap(), auth);
    assert_eq!(
        fs::read_to_string(case.codex_home.join("config.toml")).unwrap(),
        PERSONAL_CONFIG
    );
}

#[test]
fn guided_name_prompt_retries_collisions_and_invalid_names_for_both_types() {
    for kind in ["chatgpt", "api"] {
        let case = Case::new();
        case.seed("personal@example.com", "personal", "personal");
        let auth = fs::read(case.codex_home.join("auth.json")).unwrap();
        let fresh = case.home.join("fresh.json");
        write_auth(
            &fresh,
            "personal@other.example",
            "new-user",
            "new-account",
            "new-token",
        );
        let mut command = case.command();
        command.env("FAKE_AUTH", &fresh);
        if kind == "chatgpt" {
            command.args(["add", "--type", kind, "--device-auth"]);
        } else {
            command.args([
                "add",
                "--type",
                kind,
                "--base-url",
                "https://personal/v1",
                "--api-type",
                "generic",
                "--website",
                "https://personal/",
            ]);
        }
        let mut enrollment = PtyChild::spawn(command);
        if kind == "api" {
            enrollment.wait_for_output(b"API key:");
            enrollment.send(b"dummy-key\n");
        }
        enrollment.wait_for_output(b"Account name [personal-2]:");
        enrollment.send(b"PERSONAL\n");
        enrollment.wait_for_output(b"already in use");
        enrollment.send(b"123\n");
        enrollment.wait_for_output(b"not be purely numeric");
        enrollment.send(b"\n");
        enrollment.wait_success();
        assert_eq!(
            fs::read_to_string(case.store.join("profile-2/name")).unwrap(),
            "personal-2"
        );
        assert_eq!(fs::read(case.codex_home.join("auth.json")).unwrap(), auth);
    }
}

#[test]
fn name_option_can_precede_interactive_account_type_selection() {
    let case = Case::new();
    let fresh = case.home.join("fresh.json");
    write_auth(
        &fresh,
        "new@example.com",
        "new-user",
        "new-account",
        "new-token",
    );
    let mut command = case.command();
    command
        .env("FAKE_AUTH", &fresh)
        .args(["add", "--name", "work"]);
    let mut enrollment = PtyChild::spawn(command);
    enrollment.wait_for_output(b"Account type [1/2]:");
    enrollment.send(b"1\n");
    enrollment.wait_for_output(b"Login method [browser/device, Enter=browser]:");
    enrollment.send(b"\n");
    enrollment.wait_success();
    assert!(!String::from_utf8_lossy(&enrollment.output).contains("Account name ["));
    assert_eq!(
        fs::read_to_string(case.store.join("profile-1/name")).unwrap(),
        "work"
    );
}

#[test]
fn successive_api_switches_restore_the_original_oauth_route() {
    let case = Case::new();
    case.seed("personal@example.com", "personal", "personal");
    fs::write(case.codex_home.join("config.toml"), PERSONAL_CONFIG).unwrap();
    let (company_url, company_server) = api_server(&[("200 OK", MODEL_LIST)]);
    let (other_url, other_server) = api_server(&[("200 OK", MODEL_LIST)]);
    assert_success(&case.add_api("company", &company_url, "first-key"));
    assert_success(&case.add_api("other", &other_url, "second-key"));
    assert_success(&case.run(&["company"]));
    assert_eq!(
        read_config(&case)["model_providers"]["unicodex"]["name"],
        "company"
    );
    assert_success(&case.run(&["other"]));
    assert_eq!(
        read_config(&case)["model_providers"]["unicodex"]["name"],
        "other"
    );
    company_server.join().unwrap();
    other_server.join().unwrap();
    assert_eq!(
        read_config(&case)["model_providers"]["unicodex"]["experimental_bearer_token"],
        "second-key"
    );
    assert_success(&case.run(&["personal"]));
    let expected: Value = toml_edit::de::from_str(PERSONAL_CONFIG).unwrap();
    assert_eq!(read_config(&case), expected);
}

#[test]
fn legacy_api_connection_state_ignores_old_relay_snapshot_when_selecting_oauth() {
    let case = Case::new();
    case.seed("personal@example.com", "personal", "personal");
    fs::write(case.codex_home.join("config.toml"), PERSONAL_CONFIG).unwrap();
    let (url, server) = api_server(&[("200 OK", MODEL_LIST), ("200 OK", MODEL_LIST)]);
    assert_success(&case.add_api("company", &url, "dummy-key"));
    assert_success(&case.run(&["use", "company"]));
    let config_path = case.codex_home.join("config.toml");
    let mut document = fs::read_to_string(&config_path)
        .unwrap()
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    let original: Value = toml_edit::de::from_str(PERSONAL_CONFIG).unwrap();
    document["model_providers"]["unicodex"]["name"] = toml_edit::value(
        original["model_providers"]["unicodex"]["name"]
            .as_str()
            .unwrap(),
    );
    fs::write(&config_path, document.to_string()).unwrap();
    let state_path = case.store.join("connection.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    state.as_object_mut().unwrap().remove("oauth_name");
    state["api"].as_object_mut().unwrap().remove("name");
    state["oauth"] = Value::String("[model_providers.unicodex]\nname = \"old-relay\"\nbase_url = \"https://old-relay.example/v1\"\nenv_key = \"OLD_RELAY_KEY\"\nrequires_openai_auth = true\n".into());
    fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    assert_success(&case.run(&["status"]));
    assert_success(&case.run(&["use", "company"]));
    server.join().unwrap();
    assert_eq!(
        read_config(&case)["model_providers"]["unicodex"]["name"],
        "company"
    );
    assert_success(&case.run(&["use", "personal"]));
    assert_eq!(read_config(&case), original);
}

#[test]
fn builtin_openai_switch_keeps_default_provider_and_restores_login() {
    let case = Case::new();
    case.seed("personal@example.com", "personal", "personal");
    fs::write(
        case.codex_home.join("config.toml"),
        "# defaults\nmodel_reasoning_effort = \"high\"\n",
    )
    .unwrap();
    let auth = fs::read(case.codex_home.join("auth.json")).unwrap();
    let (base_url, server) = api_server(&[("200 OK", MODEL_LIST)]);
    assert_success(&case.add_api("company", &base_url, "company-key"));
    assert_success(&case.run(&["company"]));
    server.join().unwrap();
    assert!(read_config(&case).get("model_provider").is_none());
    assert_eq!(
        read_config(&case)["openai_base_url"],
        base_url.trim_end_matches('/')
    );
    let api_auth: Value =
        serde_json::from_slice(&fs::read(case.codex_home.join("auth.json")).unwrap()).unwrap();
    assert_eq!(api_auth["OPENAI_API_KEY"], "company-key");
    assert_eq!(
        fs::read(case.store.join("profile-1/auth.json")).unwrap(),
        auth
    );
    assert!(
        String::from_utf8_lossy(&case.run(&["status"]).stdout).contains("* company · CPA API key")
    );
    assert_success(&case.run(&["personal"]));
    assert_eq!(fs::read(case.codex_home.join("auth.json")).unwrap(), auth);
    assert!(read_config(&case).get("openai_base_url").is_none());
    assert!(read_config(&case).get("model_provider").is_none());
}

#[test]
fn connection_edits_outside_cxa_are_reported_without_overwriting_them() {
    let case = Case::new();
    case.seed("personal@example.com", "personal", "personal");
    fs::write(case.codex_home.join("config.toml"), PERSONAL_CONFIG).unwrap();
    let (base_url, server) = api_server(&[("200 OK", MODEL_LIST)]);
    assert_success(&case.add_api("company", &base_url, "company-key"));
    assert_success(&case.run(&["company"]));
    server.join().unwrap();
    let path = case.codex_home.join("config.toml");
    let edited = fs::read_to_string(&path)
        .unwrap()
        .replace(base_url.trim_end_matches('/'), "http://manual.test/v1");
    fs::write(&path, &edited).unwrap();
    let output = case.run(&["personal"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("changed outside cxa"));
    assert_eq!(fs::read_to_string(path).unwrap(), edited);
    assert_eq!(
        access_token(&case.codex_home.join("auth.json")),
        "token-one"
    );
}

#[test]
fn interrupted_switch_is_recovered_before_reading_config() {
    let case = Case::new();
    case.seed("personal@example.com", "personal", "personal");
    let auth = fs::read(case.codex_home.join("auth.json")).unwrap();
    fs::write(case.codex_home.join("config.toml"), "invalid = [").unwrap();
    fs::write(case.codex_home.join("auth.json"), "partial").unwrap();
    fs::write(case.store.join("connection.json"), "partial").unwrap();
    fs::write(
        case.store.join("switch-pending.json"),
        serde_json::to_vec(&json!({
            "config": PERSONAL_CONFIG.as_bytes(), "auth": auth, "state": null,
            "config_changed": true, "auth_changed": true,
        }))
        .unwrap(),
    )
    .unwrap();
    assert_success(&case.run(&["status"]));
    assert_eq!(
        fs::read_to_string(case.codex_home.join("config.toml")).unwrap(),
        PERSONAL_CONFIG
    );
    assert_eq!(fs::read(case.codex_home.join("auth.json")).unwrap(), auth);
    assert!(!case.store.join("connection.json").exists());
    assert!(!case.store.join("switch-pending.json").exists());
}

const MODEL_LIST: &str =
    r#"{"data":[{"id":"gpt-6.1-sol"},{"id":"gpt-6-sol"},{"id":"gpt-6.1-sol"}]}"#;

fn api_server(responses: &[(&str, &str)]) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/proxy/v1/", listener.local_addr().unwrap());
    let responses: Vec<_> = responses
        .iter()
        .map(|(status, body)| (status.to_string(), body.to_string()))
        .collect();
    let worker = thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let mut requests = Vec::new();
        for (status, body) in responses {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => panic!("API client did not connect: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                assert_eq!(stream.read(&mut byte).unwrap(), 1);
                request.push(byte[0]);
            }
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            requests.push(String::from_utf8(request).unwrap());
        }
        requests
    });
    (base_url, worker)
}

#[test]
fn unsupported_default_model_stops_api_switch_without_changing_login_or_config() {
    for model in ["gpt-5.6-sol", "gpt-6.1-astra"] {
        let case = Case::new();
        case.seed("personal@example.com", "personal", "personal");
        let config = PERSONAL_CONFIG.replace("gpt-6.1-sol", model);
        fs::write(case.codex_home.join("config.toml"), &config).unwrap();
        let auth = fs::read(case.codex_home.join("auth.json")).unwrap();
        let (base_url, server) = api_server(&[("200 OK", MODEL_LIST)]);
        assert_success(&case.add_api("company", &base_url, "secret-company-key"));

        let output = case.run(&["use", "company"]);

        server.join().unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(&format!("Default model `{model}`")));
        assert!(stderr.contains("Available models: gpt-6-sol, gpt-6.1-sol"));
        assert!(stderr.contains("no account, auth.json, or config.toml was changed"));
        assert!(!stderr.contains("secret-company-key"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("is now selected"));
        assert_eq!(fs::read(case.codex_home.join("auth.json")).unwrap(), auth);
        assert_eq!(
            fs::read_to_string(case.codex_home.join("config.toml")).unwrap(),
            config
        );
        assert!(!case.store.join("connection.json").exists());
        assert!(!case.store.join("switch-pending.json").exists());
    }
}

#[test]
fn models_command_queries_exact_gateway_url_and_bearer_without_switching() {
    let case = Case::new();
    case.seed("personal@example.com", "personal", "personal");
    fs::write(case.codex_home.join("config.toml"), PERSONAL_CONFIG).unwrap();
    let auth = fs::read(case.codex_home.join("auth.json")).unwrap();
    let (base_url, server) = api_server(&[("200 OK", MODEL_LIST)]);
    assert_success(&case.add_api("company", &base_url, "secret-company-key"));

    let output = case.run(&["models", "company"]);

    assert_success(&output);
    let requests = server.join().unwrap();
    assert!(requests[0].starts_with("GET /proxy/v1/models HTTP/1.1\r\n"));
    assert!(
        requests[0]
            .to_lowercase()
            .contains("authorization: bearer secret-company-key\r\n")
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Gateway models for company:\n  gpt-6-sol\n  gpt-6.1-sol\n"
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("secret-company-key"));
    assert_eq!(fs::read(case.codex_home.join("auth.json")).unwrap(), auth);
    assert_eq!(
        fs::read_to_string(case.codex_home.join("config.toml")).unwrap(),
        PERSONAL_CONFIG
    );
    assert!(!case.store.join("connection.json").exists());
}

#[test]
fn models_command_defaults_to_selected_api_account() {
    let case = Case::new();
    case.seed("personal@example.com", "personal", "personal");
    fs::write(case.codex_home.join("config.toml"), PERSONAL_CONFIG).unwrap();
    let (base_url, server) = api_server(&[("200 OK", MODEL_LIST), ("200 OK", MODEL_LIST)]);
    assert_success(&case.add_api("company", &base_url, "secret-company-key"));
    assert_success(&case.run(&["use", "company"]));
    let auth = fs::read(case.codex_home.join("auth.json")).unwrap();
    let config = fs::read(case.codex_home.join("config.toml")).unwrap();

    let output = case.run(&["models"]);

    assert_success(&output);
    assert_eq!(server.join().unwrap().len(), 2);
    assert!(String::from_utf8_lossy(&output.stdout).contains("Gateway models for company"));
    assert_eq!(fs::read(case.codex_home.join("auth.json")).unwrap(), auth);
    assert_eq!(
        fs::read(case.codex_home.join("config.toml")).unwrap(),
        config
    );
}

#[test]
fn models_command_does_not_query_chatgpt_account_as_api() {
    let case = Case::new();
    case.seed("personal@example.com", "personal", "personal");
    let output = case.run(&["models"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("queries API gateways only"));
}

#[test]
fn names_replace_numeric_and_partial_selectors_without_changing_live_state() {
    let case = Case::new();
    case.seed("jaxonblazers@example.com", "personal", "personal");
    fs::write(case.codex_home.join("config.toml"), PERSONAL_CONFIG).unwrap();
    assert_success(&case.add_api("company", "https://example.com/v1", "dummy-key"));
    let auth = fs::read(case.codex_home.join("auth.json")).unwrap();
    let config = fs::read(case.codex_home.join("config.toml")).unwrap();
    let oauth = fs::read(case.store.join("profile-1/auth.json")).unwrap();
    let api = fs::read(case.store.join("profile-2/api.json")).unwrap();
    assert_success(&case.run(&["rename", "jaxonblazers", "jax"]));
    assert_success(&case.run(&["rename", "company", "work"]));
    for args in [
        vec!["1"],
        vec!["use", "2"],
        vec!["remove", "2", "--yes"],
        vec!["models", "2"],
        vec!["relogin", "1"],
        vec!["use", "ja"],
        vec!["use", "jaxonblazers@example.com"],
    ] {
        let output = case.run(&args);
        assert!(!output.status.success(), "{args:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("No account named"));
    }
    for name in ["WORK", "123", "bad name", "../escape", ""] {
        assert!(!case.run(&["rename", "jax", name]).status.success());
    }
    let output = case.run(&["list"]);
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("* 1. jax · ChatGPT\n     jaxonblazers@example.com"),
        "{stdout}"
    );
    assert!(
        stdout.contains("  2. work · CPA API key\n     https://example.com/v1"),
        "{stdout}"
    );
    assert_eq!(fs::read(case.codex_home.join("auth.json")).unwrap(), auth);
    assert_eq!(
        fs::read(case.codex_home.join("config.toml")).unwrap(),
        config
    );
    assert_eq!(
        fs::read(case.store.join("profile-1/auth.json")).unwrap(),
        oauth
    );
    assert_eq!(
        fs::read(case.store.join("profile-2/api.json")).unwrap(),
        api
    );
    assert_success(&case.run(&["remove", "WORK", "--yes"]));
    assert_success(&case.run(&["use", "JAX"]));
    assert_eq!(
        fs::read_to_string(case.store.join("profile-1/name")).unwrap(),
        "jax"
    );
}

#[test]
fn legacy_name_migration_persists_collisions_across_removal_and_enrollment() {
    let case = Case::new();
    case.seed("shared@first.example", "one", "one");
    let second = case.home.join("second.json");
    write_auth(
        &second,
        "shared@second.example",
        "two",
        "two",
        "second-token",
    );
    assert_success(&case.run(&["import", second.to_str().unwrap()]));
    fs::remove_file(case.store.join("profile-1/name")).unwrap();
    fs::remove_file(case.store.join("profile-2/name")).unwrap();
    assert_success(&case.run(&["list"]));
    assert_eq!(
        fs::read_to_string(case.store.join("profile-1/name")).unwrap(),
        "shared"
    );
    assert_eq!(
        fs::read_to_string(case.store.join("profile-2/name")).unwrap(),
        "shared-2"
    );
    assert_success(&case.run(&["use", "shared-2"]));
    assert_success(&case.run(&["remove", "shared", "--yes"]));
    assert_success(&case.run(&["list"]));
    assert_eq!(
        fs::read_to_string(case.store.join("profile-2/name")).unwrap(),
        "shared-2"
    );
    assert_success(&case.add_api("shared", "https://example.com/v1", "dummy-key"));
    let output = case.add_api("SHARED-2", "https://example.com/v1", "dummy-key");
    assert!(!output.status.success());
}

#[test]
fn list_numbers_are_contiguous_after_removal_and_never_select_accounts() {
    let case = Case::new();
    case.seed("first@example.com", "first", "first");
    let second = case.home.join("second.json");
    write_auth(
        &second,
        "second@example.com",
        "second",
        "second",
        "second-token",
    );
    assert_success(&case.run(&["import", second.to_str().unwrap()]));
    assert_success(&case.add_api("company", "https://cpa.example.com/v1", "dummy-key"));
    assert_success(&case.run(&["use", "second"]));
    assert_success(&case.run(&["remove", "first", "--yes"]));
    let auth = fs::read(case.codex_home.join("auth.json")).unwrap();
    let output = case.run(&["list"]);
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let headings: Vec<_> = stdout
        .lines()
        .filter(|line| {
            line.trim_start_matches(['*', ' '])
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
        })
        .collect();
    assert_eq!(
        headings,
        ["* 1. second · ChatGPT", "  2. company · CPA API key"]
    );
    assert!(case.store.join("profile-2/auth.json").exists());
    assert!(case.store.join("profile-3/api.json").exists());
    for arguments in [
        vec!["1"],
        vec!["use", "2"],
        vec!["remove", "1", "--yes"],
        vec!["rename", "2", "work"],
    ] {
        assert!(!case.run(&arguments).status.success());
    }
    assert_eq!(fs::read(case.codex_home.join("auth.json")).unwrap(), auth);
    let status = case.run(&["status"]);
    assert_success(&status);
    assert!(String::from_utf8_lossy(&status.stdout).starts_with("* second · ChatGPT\n"));

    for index in 1..=10 {
        assert_success(&case.add_api(
            &format!("relay-{index}"),
            "https://relay.example.com/v1",
            "dummy-key",
        ));
    }
    let output = case.run(&["list"]);
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.starts_with("*  1. second · ChatGPT\n      second@example.com\n      Usage unknown")
    );
    assert!(stdout.contains(
        "  12. relay-10 · CPA API key\n      https://relay.example.com/v1\n      Usage unknown"
    ));
    assert!(!stdout.contains("[selected]"));
}

#[test]
fn model_list_failures_stop_switch_without_echoing_gateway_error_bodies() {
    for (status, body, expected) in [
        ("401 Unauthorized", "secret-company-key", "HTTP 401"),
        (
            "302 Found\r\nLocation: http://127.0.0.1:9/secret-company-key",
            "secret-company-key",
            "HTTP 302",
        ),
        ("200 OK", "secret-company-key", "no valid model list"),
        ("200 OK", r#"{"data":[]}"#, "advertised no models"),
        ("200 OK", r#"{"data":[{"id":""}]}"#, "invalid model ID"),
        (
            "200 OK",
            r#"{"data":[{"id":"gpt-6.1-sol\u001b"}]}"#,
            "invalid model ID",
        ),
    ] {
        let case = Case::new();
        case.seed("personal@example.com", "personal", "personal");
        fs::write(case.codex_home.join("config.toml"), PERSONAL_CONFIG).unwrap();
        let auth = fs::read(case.codex_home.join("auth.json")).unwrap();
        let (base_url, server) = api_server(&[(status, body)]);
        assert_success(&case.add_api("company", &base_url, "secret-company-key"));

        let output = case.run(&["company"]);

        server.join().unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected), "{stderr}");
        assert!(stderr.contains("No account or connection was switched"));
        assert!(!stderr.contains("secret-company-key"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("is now selected"));
        assert_eq!(fs::read(case.codex_home.join("auth.json")).unwrap(), auth);
        assert_eq!(
            fs::read_to_string(case.codex_home.join("config.toml")).unwrap(),
            PERSONAL_CONFIG
        );
        assert!(!case.store.join("connection.json").exists());
    }
}

#[test]
fn unreachable_model_endpoint_stops_switch_without_changing_login_or_config() {
    let case = Case::new();
    case.seed("personal@example.com", "personal", "personal");
    fs::write(case.codex_home.join("config.toml"), PERSONAL_CONFIG).unwrap();
    let auth = fs::read(case.codex_home.join("auth.json")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    drop(listener);
    assert_success(&case.add_api("company", &base_url, "secret-company-key"));

    let output = case.run(&["company"]);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("connection or TLS error"));
    assert_eq!(fs::read(case.codex_home.join("auth.json")).unwrap(), auth);
    assert_eq!(
        fs::read_to_string(case.codex_home.join("config.toml")).unwrap(),
        PERSONAL_CONFIG
    );
}

#[test]
fn list_queries_cpa_key_allocation_with_bearer_auth_and_shows_remaining() {
    let case = Case::new();
    let (base_url, server) = api_server(&[(
        "200 OK",
        r#"{"subscription":{"windows":[{"name":"Core","period_seconds":604800,"end_at":"2026-10-13T16:00:00Z","dimensions":[{"metric":"amount_usd","limit":100,"used":30},{"metric":"tokens","limit":1000,"used":800}]}]}}"#,
    )]);
    assert_success(&case.add_api("company", &base_url, "company-test-key"));
    let output = case
        .command()
        .env_remove("CXA_SKIP_USAGE_REFRESH")
        .arg("list")
        .output()
        .unwrap();
    let requests = server.join().unwrap();
    let request = &requests[0];
    assert_success(&output);
    assert!(
        request.starts_with(
            "GET /proxy/v0/resource/plugins/cpa-key-billing/subscription HTTP/1.1\r\n"
        )
    );
    assert!(
        request
            .to_lowercase()
            .contains("authorization: bearer company-test-key\r\n")
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("70% left"));
    assert!(stdout.contains("$70.00 / $100.00 left"));
    assert!(stdout.contains("20% left"));
    assert!(stdout.contains("200 tokens / 1000 tokens left"));
    assert!(stdout.contains("Core · USD"));
    assert!(stdout.contains("Core · Tokens"));
    assert!(stdout.contains("Weekly   ["));
    assert!(!stdout.contains("amount_usd"));
    assert!(!stdout.contains("resets at "));
    let cached: serde_json::Value =
        serde_json::from_slice(&fs::read(case.store.join("profile-1/usage.json")).unwrap())
            .unwrap();
    assert_eq!(cached["cpa"]["dimensions"][0]["window_name"], "Core");
    assert_eq!(cached["cpa"]["dimensions"][0]["period_seconds"], 604800);
    assert_eq!(cached["cpa"]["dimensions"][0]["resets_at"], 1791907200_i64);
    assert!(!stdout.contains("company-test-key"));
}

#[test]
fn cpa_http_failure_reports_the_failure_and_marks_cached_quota_stale() {
    let case = Case::new();
    let (base_url, server) = api_server(&[("401 Unauthorized", "secret-company-key")]);
    assert_success(&case.add_api("company", &base_url, "secret-company-key"));
    let now = cxa::account_store::now_epoch();
    fs::write(
        case.store.join("profile-1/usage.json"),
        serde_json::to_vec(&json!({
            "observed_at": now - 600, "last_attempted_at": now - 600, "error": null,
            "cpa": {"unlimited": true, "dimensions": []},
        }))
        .unwrap(),
    )
    .unwrap();
    let output = case
        .command()
        .env_remove("CXA_SKIP_USAGE_REFRESH")
        .arg("list")
        .output()
        .unwrap();
    server.join().unwrap();
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Last refresh failed: CPA quota request failed (HTTP 401)."));
    assert!(stdout.contains("Showing cached quota."));
    assert!(!stdout.contains("secret-company-key"));
}

#[test]
fn oauth_quota_uses_personal_route_while_company_is_selected() {
    let case = Case::new();
    case.seed("personal@example.com", "personal", "personal");
    fs::write(case.codex_home.join("config.toml"), PERSONAL_CONFIG).unwrap();
    let (base_url, server) = api_server(&[
        ("200 OK", MODEL_LIST),
        ("200 OK", r#"{"subscription":{"unlimited":true}}"#),
    ]);
    assert_success(&case.add_api("company", &base_url, "secret-company-key"));
    assert_success(&case.run(&["company"]));
    let codex = case.home.join("quota-codex");
    write_executable(
        &codex,
        r#"#!/bin/sh
case "$CODEX_HOME" in
  "$CXA_ACCOUNT_STORE"/.quota-*)
    grep -q 'requires_openai_auth = true' "$CODEX_HOME/config.toml" || exit 5
    grep -q 'name = "OpenAI"' "$CODEX_HOME/config.toml" || exit 6
    if grep -q 'base_url\|env_key\|experimental_bearer_token' "$CODEX_HOME/config.toml"; then exit 6; fi
    if grep -q secret-company-key "$CODEX_HOME/config.toml"; then exit 7; fi
    while IFS= read -r line; do
      case "$line" in
        *'"id":0'*) printf '%s\n' '{"id":0,"result":{}}' ;;
        *'"id":1'*) printf '%s\n' '{"id":1,"result":{}}' ;;
        *'"id":2'*) printf '%s\n' '{"id":2,"result":{"rateLimits":{"planType":"plus","primary":{"usedPercent":25,"windowDurationMins":300}}}}' ;;
      esac
    done
    ;;
  *)
    while IFS= read -r line; do
      case "$line" in
        *'"id":0'*) printf '%s\n' '{"id":0,"result":{}}' ;;
        *'"method":"config/read"'*) printf '%s\n' '{"id":1,"result":{"config":{"cli_auth_credentials_store":"file"}}}' ;;
      esac
    done
    ;;
esac
"#,
    );
    let auth = fs::read(case.codex_home.join("auth.json")).unwrap();
    let output = case
        .command()
        .env_remove("CXA_SKIP_USAGE_REFRESH")
        .env("CXA_CODEX_BIN", codex)
        .arg("list")
        .output()
        .unwrap();
    server.join().unwrap();
    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("75% left"));
    assert!(stdout.contains("* 2. company · CPA API key"));
    assert!(stdout.contains("CPA allocation  Unlimited"));
    assert!(!stdout.contains("quota unavailable"));
    assert_eq!(fs::read(case.codex_home.join("auth.json")).unwrap(), auth);
    assert_eq!(
        read_config(&case)["model_providers"]["unicodex"]["experimental_bearer_token"],
        "secret-company-key"
    );
}

#[test]
fn watch_cancels_a_pending_cpa_request_promptly() {
    let case = Case::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    assert_success(&case.add_api(
        "company",
        &format!("http://{}/v1", listener.local_addr().unwrap()),
        "dummy-key",
    ));
    let (sender, receiver) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            assert_eq!(stream.read(&mut byte).unwrap(), 1);
            request.push(byte[0]);
        }
        sender.send(()).unwrap();
        // The connection should close on cancellation, without a response.
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });
    let mut command = case.command();
    command.env_remove("CXA_SKIP_USAGE_REFRESH").arg("watch");
    let mut watch = PtyChild::spawn(command);
    watch.wait_for_output(b"loading");
    receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    let started = Instant::now();
    watch.send(b"q");
    watch.wait_success();
    assert!(started.elapsed() < Duration::from_secs(2));
    watch.assert_terminal_restored();
    server.join().unwrap();
}
