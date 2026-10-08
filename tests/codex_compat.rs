//! Explicit, offline compatibility check against an installed Codex app-server.
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use cxa::account_store::Store;
use cxa::api_account::ApiAccount;
use cxa::config::Config;
use serde_json::{Value, json};

struct Server {
    child: Child,
    input: ChildStdin,
    output: Receiver<String>,
}

impl Server {
    fn start(binary: &Path, home: &Path) -> Self {
        let mut child = Command::new(binary)
            .arg("app-server")
            .env("CODEX_HOME", home)
            .env_remove("CODEX_ACCESS_TOKEN")
            .env_remove("CODEX_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, output) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                    _ => break,
                }
            }
        });
        let mut server = Self {
            child,
            input,
            output,
        };
        server.request(
            0,
            "initialize",
            json!({"clientInfo": {"name":"cxa_test", "version":"1"}}),
        );
        writeln!(server.input, "{}", json!({"method":"initialized"})).unwrap();
        server
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        writeln!(
            self.input,
            "{}",
            json!({"id":id,"method":method,"params":params})
        )
        .unwrap();
        loop {
            let line = self
                .output
                .recv_timeout(Duration::from_secs(15))
                .expect("Codex did not respond");
            let response: Value = serde_json::from_str(&line).unwrap();
            if response.get("id").and_then(Value::as_u64) == Some(id) {
                assert!(
                    response.get("error").is_none(),
                    "Codex rejected {method}: {response}"
                );
                return response["result"].clone();
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "set CXA_REAL_CODEX_BIN and run explicitly; uses only isolated homes and dummy keys"]
fn actual_app_server_loads_both_api_connection_modes_without_provider_changes() {
    let binary = std::env::var_os("CXA_REAL_CODEX_BIN").expect("CXA_REAL_CODEX_BIN is required");
    let root = tempfile::tempdir().unwrap();
    for provider in ["openai", "unicodex"] {
        let home = root.path().join(provider);
        fs::create_dir_all(&home).unwrap();
        if provider == "unicodex" {
            fs::write(
                home.join("config.toml"),
                r#"model_provider = "unicodex"
[model_providers.unicodex]
name = "OpenAI"
requires_openai_auth = true
wire_api = "responses"
"#,
            )
            .unwrap();
        }
        let config = Config {
            codex_home: home.clone(),
            account_store: root.path().join(format!("{provider}-accounts")),
            switch_lock: root.path().join(format!("{provider}.lock")),
            session_auth: home.join("auth.json"),
            codex_binary: None,
            usage_ttl_seconds: 120,
            skip_usage_refresh: true,
        };
        let store = Store::new(config);
        let _lock = store.lock().unwrap();
        let profile = store
            .enroll_api(
                ApiAccount::new(
                    "company".into(),
                    "http://127.0.0.1:9/v1".into(),
                    "dummy-offline-key".into(),
                )
                .unwrap(),
            )
            .unwrap();
        store.select(profile.slot).unwrap();

        let mut server = Server::start(Path::new(&binary), &home);
        let result = server.request(1, "config/read", json!({"includeLayers":false}));
        if provider == "unicodex" {
            assert_eq!(result["config"]["model_provider"], provider);
            assert_eq!(
                result["config"]["model_providers"][provider]["base_url"],
                "http://127.0.0.1:9/v1"
            );
            assert_eq!(
                result["config"]["model_providers"][provider]["requires_openai_auth"],
                false
            );
            assert!(!home.join("auth.json").exists());
        } else {
            assert!(result["config"]["model_provider"].is_null());
            assert_eq!(result["config"]["openai_base_url"], "http://127.0.0.1:9/v1");
            let account = server.request(2, "account/read", json!({"refreshToken":false}));
            assert_eq!(account["account"]["type"], "apiKey");
        }
    }
}
