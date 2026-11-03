//! Explicit, offline compatibility check against an installed Codex app-server.
use std::fmt::Write as _;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

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
            .env("HOME", home)
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

#[test]
#[ignore = "set CXA_REAL_CODEX_BIN and run explicitly; uses only isolated homes and dummy keys"]
fn actual_codex_preserves_chatgpt_identity_and_uses_only_the_gateway_key_for_inference() {
    let binary = std::env::var_os("CXA_REAL_CODEX_BIN").expect("CXA_REAL_CODEX_BIN is required");
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("codex");
    fs::create_dir(&home).unwrap();
    fs::write(
        home.join("config.toml"),
        r#"model_provider = "unicodex"
model = "gpt-6.1-sol"
cli_auth_credentials_store = "file"
chatgpt_base_url = "http://127.0.0.1:9"
[model_providers.unicodex]
name = "OpenAI"
requires_openai_auth = true
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
"#,
    )
    .unwrap();
    let claims = json!({
        "email": "personal@example.invalid", "exp": 4102444800u64,
        "https://api.openai.com/auth": {
            "chatgpt_user_id": "personal", "chatgpt_account_id": "personal",
            "chatgpt_plan_type": "plus"
        }
    });
    let token = format!(
        "header.{}.signature",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap())
    );
    let auth = serde_json::to_vec(&json!({
        "auth_mode": "chatgpt", "last_refresh": chrono::Utc::now().to_rfc3339(),
        "tokens": {"id_token": token, "access_token": "dummy-chatgpt-secret",
                   "refresh_token": "dummy-refresh", "account_id": "personal"}
    }))
    .unwrap();
    fs::write(home.join("auth.json"), &auth).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let store = Store::new(Config {
        codex_home: home.clone(),
        account_store: root.path().join("accounts"),
        switch_lock: root.path().join("switch.lock"),
        session_auth: home.join("auth.json"),
        codex_binary: None,
        usage_ttl_seconds: 0,
        skip_usage_refresh: true,
    });
    let _lock = store.lock().unwrap();
    let profile = store
        .enroll_api(
            ApiAccount::new(
                "company".into(),
                base_url,
                "dummy-company-key".into(),
                cxa::api_account::ApiType::Generic,
                None,
            )
            .unwrap(),
            None,
        )
        .unwrap();
    store.select(profile.slot).unwrap();
    assert_eq!(fs::read(home.join("auth.json")).unwrap(), auth);
    let mut server = Server::start(Path::new(&binary), &home);
    let status = server.request(
        1,
        "getAuthStatus",
        json!({"includeToken":false, "refreshToken":false}),
    );
    assert_eq!(status["authMethod"], "chatgpt");
    assert_eq!(status["requiresOpenaiAuth"], true);
    drop(server);

    let gateway = thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(error) => panic!("Codex did not connect to the gateway: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            headers.push_str(&line);
            if line == "\r\n" {
                break;
            }
        }
        let length: usize = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|length| length.trim().parse().unwrap())
            })
            .unwrap();
        reader.read_exact(&mut vec![0; length]).unwrap();
        let message = json!({"id":"msg_probe", "type":"message", "role":"assistant", "status":"completed",
            "content":[{"type":"output_text", "text":"OK", "annotations":[]}]});
        let events = [
            json!({"type":"response.created", "response":{"id":"resp_probe", "status":"in_progress", "output":[]}}),
            json!({"type":"response.output_item.added", "output_index":0, "item":{"id":"msg_probe", "type":"message", "role":"assistant", "content":[]}}),
            json!({"type":"response.output_text.delta", "item_id":"msg_probe", "output_index":0, "content_index":0, "delta":"OK"}),
            json!({"type":"response.output_item.done", "output_index":0, "item":message}),
            json!({"type":"response.completed", "response":{"id":"resp_probe", "status":"completed", "output":[message],
                "usage":{"input_tokens":1, "output_tokens":1, "total_tokens":2}}}),
        ];
        let mut body = String::new();
        for event in events {
            writeln!(body, "data: {event}\n").unwrap();
        }
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        headers
    });
    let output = Command::new(&binary)
        .args(["exec", "--skip-git-repo-check", "--json", "Reply with OK."])
        .current_dir(&home)
        .env("HOME", root.path())
        .env("CODEX_HOME", &home)
        .env_remove("CODEX_ACCESS_TOKEN")
        .env_remove("CODEX_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .output()
        .unwrap();
    let headers = gateway.join().unwrap().to_ascii_lowercase();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(headers.starts_with("post /v1/responses "));
    assert!(headers.contains("authorization: bearer dummy-company-key\r\n"));
    assert!(!headers.contains("dummy-chatgpt-secret"));
    assert!(!headers.contains("chatgpt-account-id:"));
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
                    cxa::api_account::ApiType::Generic,
                    None,
                )
                .unwrap(),
                None,
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
        drop(server);
        let oauth_home = root.path().join(format!("{provider}-oauth"));
        fs::create_dir_all(&oauth_home).unwrap();
        cxa::connection::copy_oauth_config(&store.config, &oauth_home.join("config.toml")).unwrap();
        let mut oauth_server = Server::start(Path::new(&binary), &oauth_home);
        let official = oauth_server.request(1, "config/read", json!({"includeLayers":false}));
        assert!(official["config"]["openai_base_url"].is_null());
        if provider == "unicodex" {
            assert_eq!(official["config"]["model_provider"], "unicodex");
            let table = &official["config"]["model_providers"]["unicodex"];
            assert_eq!(table["name"], "OpenAI");
            assert_eq!(table["requires_openai_auth"], true);
            assert!(table["base_url"].is_null());
            assert!(table["experimental_bearer_token"].is_null());
        } else {
            assert!(official["config"]["model_provider"].is_null());
        }
    }
}
