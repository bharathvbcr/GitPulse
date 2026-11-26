//! Live end-to-end integration test exercising the real Go `gitpulse-relay`
//! binary alongside Rust `gitpulsed`, `gitpulse-protocol`, `HostNoiseManager`,
//! `RevocationCache`, `ApprovalBroker`, and `SteeringIngressGate` over real
//! loopback TCP/HTTP/WebSocket connections.
//!
//! Runs against `GITPULSE_RELAY_DIR` (defaulting to
//! `/Users/bharath/Code/devtools/gitpulse-relay.remote-control`), skipping
//! gracefully ONLY when the `go` toolchain or the relay checkout is absent.

mod common;

use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use gitpulse_lib::alerts::approval_broker::{
    ApprovalBroker, BrokerError, HookBrokerReply, MockPresenceProvider, RegisterApprovalRequest,
    ResolveStatus, TrustedApprovalKeyStore, UserPresence,
};
use gitpulse_lib::procguard::LockedSpawn;
use gitpulse_lib::remote::config::{
    RemoteControlConfig, Tier, EXIT_CODE_FREE_OR_DISABLED, RELAY_LOCK_FILE, REMOTE_CONFIG_FILE,
};
use gitpulse_lib::remote::revocations::{
    issue_device_certificate, issue_revocation_list, DeviceCertificate, DeviceKind,
    RevocationCache, SignedRevocationList,
};
use gitpulse_lib::remote::tunnel::{
    sign_relay_request, ApprovalChannel, ApprovalDecisionKind, ApprovalTicket, CapabilitySet,
    ControlPlaneEvent, FrameClass, HostNoiseManager, NoiseHandshake, OuterFrame, SessionSnapshot,
    SteeringIngressGate, SteeringIngressVerdict, GPTF_VERSION, WS_CLOSE_REVOKED_OR_DOWNGRADED,
};
use gitpulse_protocol::canonical::steering_text_sha256_hex;
use gitpulse_protocol::crypto::{
    ed25519_public_from_secret, sign_approval_decision_software, sign_steering_confirm_software,
    x25519_public_from_secret,
};
use gitpulse_protocol::wire::SteeringConfirmReq;
use gitpulse_protocol::ProtocolError;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{mpsc as std_mpsc, Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::Message;

const DEFAULT_RELAY_DIR: &str = "/Users/bharath/Code/devtools/gitpulse-relay.remote-control";

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_secs() as i64
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_millis() as i64
}

/// Monotonic millisecond timestamp generator so consecutive Ed25519-signed
/// relay requests never collide in the relay's replay cache.
struct MonotonicClock {
    last_ms: AtomicI64,
}

impl MonotonicClock {
    fn new() -> Self {
        Self {
            last_ms: AtomicI64::new(now_unix_ms()),
        }
    }

    fn next_ms(&self) -> i64 {
        let now = now_unix_ms();
        loop {
            let prev = self.last_ms.load(Ordering::SeqCst);
            let next = now.max(prev + 1);
            if self
                .last_ms
                .compare_exchange(prev, next, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return next;
            }
        }
    }
}

/// Deterministic P-256 test keypair from `steering_confirm_v1.json`
/// (secret scalar `0x66 * 32`, SEC1 compressed pubkey
/// `Awu7xei8hL0z0dPOA/+smnR/TBmT/dsuyTpBFqhvAip3`).
fn kat_p256_keypair() -> ([u8; 32], Vec<u8>) {
    let secret = [0x66u8; 32];
    let pub_sec1 = base64::engine::general_purpose::STANDARD
        .decode("Awu7xei8hL0z0dPOA/+smnR/TBmT/dsuyTpBFqhvAip3")
        .expect("decode kat p256 sec1");
    (secret, pub_sec1)
}

fn locate_relay_dir() -> Option<PathBuf> {
    let candidate = std::env::var_os("GITPULSE_RELAY_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_RELAY_DIR));
    if candidate.join("cmd/gitpulse-relay/main.go").is_file() {
        Some(candidate)
    } else {
        None
    }
}

fn go_toolchain_available() -> bool {
    Command::new("go")
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status_locked()
        .is_ok_and(|s| s.success())
}

/// Applies real user Go cache/module paths so the hermetic test `HOME` does
/// not force `go build` to re-download modules from the network.
fn configure_go_env(cmd: &mut Command) {
    if let Some(real_home) = common::env::hermetic::real_home() {
        if std::env::var_os("GOPATH").is_none() {
            cmd.env("GOPATH", real_home.join("go"));
        }
        if std::env::var_os("GOMODCACHE").is_none() {
            cmd.env("GOMODCACHE", real_home.join("go").join("pkg").join("mod"));
        }
        if std::env::var_os("GOCACHE").is_none() {
            #[cfg(target_os = "macos")]
            let cache = real_home.join("Library").join("Caches").join("go-build");
            #[cfg(not(target_os = "macos"))]
            let cache = real_home.join(".cache").join("go-build");
            cmd.env("GOCACHE", cache);
        }
    }
}

fn build_relay_binary(relay_dir: &Path, out_dir: &Path) -> PathBuf {
    let bin_path = out_dir.join(if cfg!(windows) {
        "gitpulse-relay.exe"
    } else {
        "gitpulse-relay"
    });
    let mut cmd = Command::new("go");
    configure_go_env(&mut cmd);
    let output = cmd
        .current_dir(relay_dir)
        .args(["build", "-o"])
        .arg(&bin_path)
        .arg("./cmd/gitpulse-relay")
        .output_locked()
        .expect("spawn go build for gitpulse-relay");
    assert!(
        output.status.success(),
        "go build gitpulse-relay failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    bin_path
}

struct RelayProcess {
    child: Child,
    addr: String,
    stderr_log: Arc<Mutex<Vec<String>>>,
}

impl RelayProcess {
    fn spawn(bin: &Path, data_dir: &Path, github_api_base: &str) -> Self {
        let mut child = Command::new(bin)
            .args([
                "serve",
                "--addr",
                "127.0.0.1:0",
                "--data-dir",
                data_dir.to_str().expect("utf8 data_dir"),
                "--store-backend",
                "sqlite",
                "--github-api-base",
                github_api_base,
                "--paywall-mode",
                "enforced",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn_locked()
            .expect("spawn gitpulse-relay serve");

        let stderr = child.stderr.take().expect("relay stderr pipe");
        let stderr_log = Arc::new(Mutex::new(Vec::<String>::new()));
        let log_clone = Arc::clone(&stderr_log);
        let (addr_tx, addr_rx) = std_mpsc::channel::<String>();

        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            let mut sent = false;
            for line in reader.lines().map_while(Result::ok) {
                if !sent && line.contains("msg=listening") {
                    if let Some(addr) = parse_slog_field(&line, "addr") {
                        let _ = addr_tx.send(addr);
                        sent = true;
                    }
                }
                if let Ok(mut guard) = log_clone.lock() {
                    if guard.len() >= 256 {
                        guard.remove(0);
                    }
                    guard.push(line);
                }
            }
        });

        let wait_limit = common::env::coverage_relaxed(Duration::from_secs(15));
        let addr = match addr_rx.recv_timeout(wait_limit) {
            Ok(addr) => addr,
            Err(err) => {
                let logs = stderr_log.lock().map(|g| g.join("\n")).unwrap_or_default();
                let _ = child.kill();
                let _ = child.wait();
                panic!("gitpulse-relay did not emit listening address ({err}):\n{logs}");
            }
        };

        Self {
            child,
            addr,
            stderr_log,
        }
    }

    fn dump_logs(&self) -> String {
        self.stderr_log
            .lock()
            .map(|g| g.join("\n"))
            .unwrap_or_default()
    }
}

impl Drop for RelayProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn parse_slog_field(line: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=");
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    if let Some(quoted) = rest.strip_prefix('"') {
        let end = quoted.find('"')?;
        Some(quoted[..end].to_string())
    } else {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        Some(rest[..end].to_string())
    }
}

/// Minimal in-process mock GitHub `/user` endpoint for `POST /v1/auth/github`.
async fn start_mock_github_server(
    expected_token: String,
    user_id: i64,
    login: String,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock github listener");
    let addr = listener.local_addr().expect("mock github local_addr");
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let expected = expected_token.clone();
            let login_val = login.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let Ok(n) = stream.read(&mut buf).await else {
                    return;
                };
                let req = String::from_utf8_lossy(&buf[..n]);
                let auth_ok = req.lines().any(|l| {
                    l.to_ascii_lowercase().starts_with("authorization:")
                        && l.contains(&format!("Bearer {expected}"))
                });
                let (status, body) = if req.starts_with("GET /user ") && auth_ok {
                    (
                        "200 OK",
                        json!({
                            "id": user_id,
                            "login": login_val,
                            "avatar_url": "https://example.invalid/e2e-avatar.png"
                        })
                        .to_string(),
                    )
                } else {
                    (
                        "401 Unauthorized",
                        r#"{"message":"Bad credentials"}"#.into(),
                    )
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    (addr, handle)
}

#[derive(Debug)]
struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

impl HttpResponse {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!(
                "invalid JSON response (status {}): {e}\nbody: {}",
                self.status,
                self.text()
            )
        })
    }
}

async fn http_request(
    addr: &str,
    method: &str,
    path_and_query: &str,
    headers: &[(&str, String)],
    body: &[u8],
) -> HttpResponse {
    let mut stream = TcpStream::connect(addr)
        .await
        .unwrap_or_else(|e| panic!("tcp connect {addr}: {e}"));
    let mut raw = format!(
        "{method} {path_and_query} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        raw.push_str(&format!("{k}: {v}\r\n"));
    }
    raw.push_str("\r\n");
    stream.write_all(raw.as_bytes()).await.expect("write req");
    if !body.is_empty() {
        stream.write_all(body).await.expect("write body");
    }
    stream.flush().await.expect("flush req");

    let mut resp_bytes = Vec::new();
    stream
        .read_to_end(&mut resp_bytes)
        .await
        .expect("read http response");
    let header_end = resp_bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("http response header terminator");
    let header_text = String::from_utf8_lossy(&resp_bytes[..header_end]);
    let status_line = header_text.lines().next().expect("http status line");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .expect("http status code token")
        .parse()
        .expect("parse http status code");
    let is_chunked = header_text.lines().any(|l| {
        let lower = l.to_ascii_lowercase();
        lower.starts_with("transfer-encoding:") && lower.contains("chunked")
    });
    let raw_body = &resp_bytes[header_end + 4..];
    let body = if is_chunked {
        decode_chunked(raw_body)
    } else {
        raw_body.to_vec()
    };
    HttpResponse { status, body }
}

fn decode_chunked(mut input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(pos) = input.windows(2).position(|w| w == b"\r\n") {
        let size_str = std::str::from_utf8(&input[..pos])
            .expect("chunk size utf8")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        let size = usize::from_str_radix(size_str, 16).expect("chunk size hex");
        input = &input[pos + 2..];
        if size == 0 {
            break;
        }
        out.extend_from_slice(&input[..size]);
        input = &input[size + 2..];
    }
    out
}

async fn signed_http_request(
    addr: &str,
    clock: &MonotonicClock,
    device_id: &str,
    ed25519_seed: &[u8; 32],
    method: &str,
    path_and_query: &str,
    body: &[u8],
) -> HttpResponse {
    let auth = sign_relay_request(
        device_id,
        ed25519_seed,
        method,
        path_and_query,
        clock.next_ms(),
        body,
    )
    .expect("sign relay http request");
    let pairs = auth.pairs();
    let mut headers: Vec<(&str, String)> = pairs.into_iter().collect();
    if !body.is_empty() {
        headers.push(("Content-Type", "application/json".into()));
    }
    http_request(addr, method, path_and_query, &headers, body).await
}

type LiveWs = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

async fn connect_signed_ws(
    addr: &str,
    clock: &MonotonicClock,
    device_id: &str,
    ed25519_seed: &[u8; 32],
    request_uri: &str,
) -> Result<LiveWs, (u16, String)> {
    let auth = sign_relay_request(
        device_id,
        ed25519_seed,
        "GET",
        request_uri,
        clock.next_ms(),
        &[],
    )
    .expect("sign relay ws upgrade");
    let full_url = format!("ws://{addr}{request_uri}");
    let mut req = full_url.as_str().into_client_request().expect("ws request");
    for (name, value) in auth.pairs() {
        req.headers_mut().insert(
            http::header::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
            value.parse().expect("header value"),
        );
    }
    match tokio_tungstenite::connect_async(req).await {
        Ok((ws, _resp)) => Ok(ws),
        Err(WsError::Http(resp)) => {
            let status = resp.status().as_u16();
            let body = resp
                .into_body()
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            Err((status, body))
        }
        Err(other) => panic!("unexpected ws connect error for {full_url}: {other}"),
    }
}

async fn recv_ws_text(ws: &mut LiveWs, label: &str) -> String {
    let limit = common::env::coverage_relaxed(Duration::from_secs(10));
    tokio::time::timeout(limit, async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(text))) => return text.to_string(),
                Some(Ok(Message::Ping(payload))) => {
                    let _ = ws.send(Message::Pong(payload)).await;
                }
                Some(Ok(Message::Pong(_))) => continue,
                other => panic!("expected text WS frame ({label}), got {other:?}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for WS text frame ({label})"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_relay_free_gate_pro_noise_ik_approvals_steering_and_4003_revocation() {
    common::unmetered_spawns();

    if !go_toolchain_available() {
        eprintln!("SKIP remote_relay_e2e: `go` toolchain not found in PATH");
        return;
    }
    let Some(relay_dir) = locate_relay_dir() else {
        eprintln!(
            "SKIP remote_relay_e2e: relay source dir not found (set GITPULSE_RELAY_DIR or checkout {DEFAULT_RELAY_DIR})"
        );
        return;
    };

    let tmp = tempfile::tempdir().expect("tempdir");
    let bin_dir = tmp.path().join("bin");
    let data_dir = tmp.path().join("relay-data");
    let free_cfg_dir = tmp.path().join("free-cfg");
    let repo_dir = tmp.path().join("trusted-repo");
    let mailbox_dir = tmp.path().join("mailbox");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&free_cfg_dir).unwrap();
    std::fs::create_dir_all(&repo_dir).unwrap();
    std::fs::create_dir_all(&mailbox_dir).unwrap();

    // 1. Build the real Go `gitpulse-relay` binary and launch with `--paywall-mode enforced`.
    let relay_bin = build_relay_binary(&relay_dir, &bin_dir);
    let github_token = "gho_e2e_live_relay_smoke_token_001".to_string();
    let (gh_addr, gh_task) =
        start_mock_github_server(github_token.clone(), 424_242, "e2e-operator".into()).await;
    let gh_base = format!("http://{gh_addr}");
    let relay = RelayProcess::spawn(&relay_bin, &data_dir, &gh_base);

    let health = http_request(&relay.addr, "GET", "/healthz", &[], &[]).await;
    assert_eq!(
        health.status,
        200,
        "relay /healthz failed: {}\nlogs:\n{}",
        health.text(),
        relay.dump_logs()
    );
    assert_eq!(health.json()["ok"], json!(true));

    let clock = MonotonicClock::new();

    // -----------------------------------------------------------------------
    // (a) Free tier enforcement: `gitpulsed --relay` exits 4 AND live relay
    //     rejects `/v1/tunnel/ws` with 403 `feature_required`.
    // -----------------------------------------------------------------------
    let free_cfg = RemoteControlConfig {
        tier: Tier::Free,
        remote_control_enabled: true,
        ..RemoteControlConfig::default()
    };
    std::fs::write(
        free_cfg_dir.join(REMOTE_CONFIG_FILE),
        serde_json::to_vec_pretty(&free_cfg).expect("serialize free remote-control.json"),
    )
    .expect("write free remote-control.json");
    let gitpulsed_out = Command::new(env!("CARGO_BIN_EXE_gitpulsed"))
        .args(["--relay", "--once", "--config-dir"])
        .arg(&free_cfg_dir)
        .output_locked()
        .expect("run real gitpulsed --relay binary");
    assert_eq!(
        gitpulsed_out.status.code(),
        Some(EXIT_CODE_FREE_OR_DISABLED),
        "expected gitpulsed --relay on free tier to exit 4, got {:?}\nstderr: {}",
        gitpulsed_out.status.code(),
        String::from_utf8_lossy(&gitpulsed_out.stderr)
    );
    assert!(
        !free_cfg_dir
            .join("gitpulsed")
            .join(RELAY_LOCK_FILE)
            .exists(),
        "free-tier refusal must exit 4 before acquiring gitpulsed-relay.lock"
    );

    let auth_body = serde_json::to_vec(&json!({ "github_token": github_token })).unwrap();
    let auth_resp = http_request(
        &relay.addr,
        "POST",
        "/v1/auth/github",
        &[("Content-Type", "application/json".into())],
        &auth_body,
    )
    .await;
    assert_eq!(auth_resp.status, 200, "auth/github: {}", auth_resp.text());
    let auth_json = auth_resp.json();
    let session_token = auth_json["session_token"]
        .as_str()
        .expect("session_token")
        .to_string();
    assert_eq!(auth_json["entitlements"]["tier"], json!("free"));
    assert_eq!(auth_json["entitlements"]["paywall_mode"], json!("enforced"));
    let free_features = auth_json["entitlements"]["features"]
        .as_array()
        .expect("features array");
    assert!(
        !free_features.iter().any(|f| f == "remote_control"),
        "free tier must not include remote_control under enforced paywall"
    );

    // Deterministic cryptographic keys for Account IdentityKey, Desktop host, Mobile client.
    let identity_seed = [0xA1u8; 32];
    let identity_pub = ed25519_public_from_secret(&identity_seed);

    let host_ed_seed = [0xB1u8; 32];
    let host_ed_pub = ed25519_public_from_secret(&host_ed_seed);
    let host_x_secret = [0xC1u8; 32];
    let host_x_pub = x25519_public_from_secret(&host_x_secret);

    let mobile_ed_seed = [0xB2u8; 32];
    let mobile_ed_pub = ed25519_public_from_secret(&mobile_ed_seed);
    let mobile_x_secret = [0xC2u8; 32];
    let mobile_x_pub = x25519_public_from_secret(&mobile_x_secret);
    let (mobile_p256_secret, mobile_p256_sec1) = kat_p256_keypair();

    let bearer = [("Authorization", format!("Bearer {session_token}"))];
    let bearer_json = [
        ("Authorization", format!("Bearer {session_token}")),
        ("Content-Type", "application/json".into()),
    ];

    let reg_host = http_request(
        &relay.addr,
        "POST",
        "/v1/devices",
        &bearer_json,
        &serde_json::to_vec(&json!({
            "name": "MacBook Pro E2E Host",
            "kind": "desktop",
            "ed25519_public_key": base64::engine::general_purpose::STANDARD.encode(host_ed_pub),
        }))
        .unwrap(),
    )
    .await;
    assert_eq!(reg_host.status, 201, "register host: {}", reg_host.text());
    let host_device_id = reg_host.json()["device_id"]
        .as_str()
        .expect("host device_id")
        .to_string();

    let reg_mobile = http_request(
        &relay.addr,
        "POST",
        "/v1/devices",
        &bearer_json,
        &serde_json::to_vec(&json!({
            "name": "iPhone 16 Pro E2E",
            "kind": "mobile",
            "ed25519_public_key": base64::engine::general_purpose::STANDARD.encode(mobile_ed_pub),
        }))
        .unwrap(),
    )
    .await;
    assert_eq!(
        reg_mobile.status,
        201,
        "register mobile: {}",
        reg_mobile.text()
    );
    let mobile_device_id = reg_mobile.json()["device_id"]
        .as_str()
        .expect("mobile device_id")
        .to_string();

    // Free account WS tunnel attempt MUST be rejected with 403 `feature_required`.
    let free_ws_err = connect_signed_ws(
        &relay.addr,
        &clock,
        &host_device_id,
        &host_ed_seed,
        "/v1/tunnel/ws?role=host&after_epoch=0&after_seq=0",
    )
    .await
    .expect_err("free account must be rejected on /v1/tunnel/ws");
    assert_eq!(
        free_ws_err.0, 403,
        "expected 403 on free account WS upgrade, got {:?}",
        free_ws_err
    );
    assert!(
        free_ws_err.1.contains("feature_required"),
        "expected feature_required in 403 body, got {:?}",
        free_ws_err.1
    );

    // -----------------------------------------------------------------------
    // (b) Upgrade to `pro` (Labs Geek), enroll desktop + mobile devices with
    //     DeviceCertificate v2 + P-256 pubkey, upload & fetch SignedRevocationList.
    // -----------------------------------------------------------------------
    let grant_out = Command::new(&relay_bin)
        .args([
            "grant",
            "--login",
            "e2e-operator",
            "--tier",
            "pro",
            "--data-dir",
            data_dir.to_str().expect("utf8 data_dir"),
            "--store-backend",
            "sqlite",
        ])
        .output_locked()
        .expect("run gitpulse-relay grant");
    assert!(
        grant_out.status.success(),
        "gitpulse-relay grant failed: {}",
        String::from_utf8_lossy(&grant_out.stderr)
    );

    let ent_resp = http_request(&relay.addr, "GET", "/v1/entitlements", &bearer, &[]).await;
    assert_eq!(ent_resp.status, 200, "entitlements: {}", ent_resp.text());
    let ent_json = ent_resp.json();
    assert_eq!(ent_json["tier"], json!("pro"));
    assert!(
        ent_json["features"]
            .as_array()
            .expect("pro features")
            .iter()
            .any(|f| f == "remote_control"),
        "pro tier must grant remote_control"
    );

    let now = now_unix_secs();
    let host_cert = issue_device_certificate(
        &identity_seed,
        &host_device_id,
        DeviceKind::Desktop,
        "MacBook Pro E2E Host",
        &host_ed_pub,
        &host_x_pub,
        None,
        now - 60,
        now + 7 * 24 * 3600,
    )
    .expect("issue host cert v2");

    let mobile_cert = issue_device_certificate(
        &identity_seed,
        &mobile_device_id,
        DeviceKind::Mobile,
        "iPhone 16 Pro E2E",
        &mobile_ed_pub,
        &mobile_x_pub,
        Some(&mobile_p256_sec1),
        now - 60,
        now + 7 * 24 * 3600,
    )
    .expect("issue mobile cert v2");

    let host_cert_bytes = serde_json::to_vec(&host_cert).unwrap();
    let mobile_cert_bytes = serde_json::to_vec(&mobile_cert).unwrap();

    // Upload Account IdentityKey + Desktop DeviceCertificate v2.
    let put_id = signed_http_request(
        &relay.addr,
        &clock,
        &host_device_id,
        &host_ed_seed,
        "PUT",
        "/v1/identity",
        &serde_json::to_vec(&json!({
            "identity_public_key": base64::engine::general_purpose::STANDARD.encode(identity_pub),
            "certificate": base64::engine::general_purpose::STANDARD.encode(&host_cert_bytes),
        }))
        .unwrap(),
    )
    .await;
    assert_eq!(put_id.status, 200, "PUT /v1/identity: {}", put_id.text());

    // Publish X25519 keys for both devices.
    for (dev_id, ed_seed, x_pub) in [
        (&host_device_id, &host_ed_seed, &host_x_pub),
        (&mobile_device_id, &mobile_ed_seed, &mobile_x_pub),
    ] {
        let resp = signed_http_request(
            &relay.addr,
            &clock,
            dev_id,
            ed_seed,
            "PUT",
            "/v1/devices/me/enc-key",
            &serde_json::to_vec(&json!({
                "x25519_public_key": base64::engine::general_purpose::STANDARD.encode(x_pub),
            }))
            .unwrap(),
        )
        .await;
        assert_eq!(
            resp.status,
            200,
            "PUT enc-key for {dev_id}: {}",
            resp.text()
        );
    }

    // Complete relay pairing enrolment flow for mobile with DeviceCertificate v2.
    let post_enrol = signed_http_request(
        &relay.addr,
        &clock,
        &mobile_device_id,
        &mobile_ed_seed,
        "POST",
        "/v1/enrolments",
        &serde_json::to_vec(&json!({
            "x25519_public_key": base64::engine::general_purpose::STANDARD.encode(mobile_x_pub),
        }))
        .unwrap(),
    )
    .await;
    assert_eq!(
        post_enrol.status,
        201,
        "POST /v1/enrolments: {}",
        post_enrol.text()
    );

    let approve_enrol = signed_http_request(
        &relay.addr,
        &clock,
        &host_device_id,
        &host_ed_seed,
        "POST",
        &format!("/v1/enrolments/{mobile_device_id}/approve"),
        &serde_json::to_vec(&json!({
            "certificate": base64::engine::general_purpose::STANDARD.encode(&mobile_cert_bytes),
            "payload": base64::engine::general_purpose::STANDARD.encode(b"opaque-sealed-pairing-blob"),
        }))
        .unwrap(),
    )
    .await;
    assert_eq!(
        approve_enrol.status,
        200,
        "POST approve enrolment: {}",
        approve_enrol.text()
    );

    let self_enrol = signed_http_request(
        &relay.addr,
        &clock,
        &mobile_device_id,
        &mobile_ed_seed,
        "GET",
        "/v1/enrolments/self",
        &[],
    )
    .await;
    assert_eq!(self_enrol.status, 200);
    let self_enrol_json = self_enrol.json();
    assert_eq!(self_enrol_json["status"], json!("approved"));
    let fetched_mobile_cert: DeviceCertificate = serde_json::from_slice(
        &base64::engine::general_purpose::STANDARD
            .decode(self_enrol_json["certificate"].as_str().unwrap())
            .expect("decode enrolled mobile cert"),
    )
    .expect("parse enrolled mobile cert");
    assert_eq!(fetched_mobile_cert, mobile_cert);

    // Issue and upload SignedRevocationList v1 via `PUT /v1/identity/revocations`.
    let rev_v1 = issue_revocation_list(&identity_seed, 1, now - 30, now + 3600, Vec::new())
        .expect("issue SignedRevocationList v1");
    let put_rev = signed_http_request(
        &relay.addr,
        &clock,
        &host_device_id,
        &host_ed_seed,
        "PUT",
        "/v1/identity/revocations",
        &serde_json::to_vec(&rev_v1).unwrap(),
    )
    .await;
    assert_eq!(
        put_rev.status,
        200,
        "PUT /v1/identity/revocations: {}",
        put_rev.text()
    );

    // Fetch SignedRevocationList back live via `GET /v1/identity/revocations`.
    let get_rev = signed_http_request(
        &relay.addr,
        &clock,
        &host_device_id,
        &host_ed_seed,
        "GET",
        "/v1/identity/revocations",
        &[],
    )
    .await;
    assert_eq!(
        get_rev.status,
        200,
        "GET /v1/identity/revocations: {}",
        get_rev.text()
    );
    let live_rev_list: SignedRevocationList =
        serde_json::from_slice(&get_rev.body).expect("parse live SignedRevocationList");
    assert_eq!(live_rev_list.version, 1);

    let mut host_rev_cache = RevocationCache::new(identity_pub, Duration::from_secs(900));
    host_rev_cache
        .update(live_rev_list.clone(), now)
        .expect("install live revocation list into host cache");

    // -----------------------------------------------------------------------
    // (c) Open live WS tunnels for host (`role=host`) and mobile (`role=client`)
    //     using Ed25519 request-signing headers from `gitpulse_protocol`.
    // -----------------------------------------------------------------------
    let mut host_ws = connect_signed_ws(
        &relay.addr,
        &clock,
        &host_device_id,
        &host_ed_seed,
        "/v1/tunnel/ws?role=host&after_epoch=0&after_seq=0",
    )
    .await
    .expect("host WS upgrade on pro tier");

    let mut mobile_ws = connect_signed_ws(
        &relay.addr,
        &clock,
        &mobile_device_id,
        &mobile_ed_seed,
        "/v1/tunnel/ws?role=client&after_epoch=0&after_seq=0",
    )
    .await
    .expect("mobile WS upgrade on pro tier");

    // Verify strict P2 role/device-kind binding: mobile cannot claim role=host,
    // desktop cannot claim role=client (403 role_device_kind_mismatch).
    let mobile_as_host = connect_signed_ws(
        &relay.addr,
        &clock,
        &mobile_device_id,
        &mobile_ed_seed,
        "/v1/tunnel/ws?role=host&after_epoch=0&after_seq=0",
    )
    .await
    .expect_err("mobile requesting role=host must be rejected");
    assert_eq!(
        mobile_as_host.0, 403,
        "expected 403 for mobile->host mismatch, got {:?}",
        mobile_as_host
    );
    assert!(
        mobile_as_host.1.contains("role_device_kind_mismatch"),
        "expected role_device_kind_mismatch for mobile->host, got {:?}",
        mobile_as_host.1
    );

    let desktop_as_client = connect_signed_ws(
        &relay.addr,
        &clock,
        &host_device_id,
        &host_ed_seed,
        "/v1/tunnel/ws?role=client&after_epoch=0&after_seq=0",
    )
    .await
    .expect_err("desktop requesting role=client must be rejected");
    assert_eq!(
        desktop_as_client.0, 403,
        "expected 403 for desktop->client mismatch, got {:?}",
        desktop_as_client
    );
    assert!(
        desktop_as_client.1.contains("role_device_kind_mismatch"),
        "expected role_device_kind_mismatch for desktop->client, got {:?}",
        desktop_as_client.1
    );

    // -----------------------------------------------------------------------
    // (d) Complete real Noise `IK` handshake over live relay WS (`GPTF` v1
    //     `OuterFrame`s).
    // -----------------------------------------------------------------------
    let host_epoch: u64 = 1;
    let mut host_noise_mgr = HostNoiseManager::new(&host_device_id, host_epoch);
    let stream_id = host_noise_mgr.stream_id();

    let mobile_rev_policy = host_rev_cache
        .enforced_policy(now)
        .expect("enforced revocation policy");
    let mut mobile_hs = NoiseHandshake::new_initiator(
        &mobile_x_secret,
        &host_cert,
        &identity_pub,
        mobile_rev_policy,
        now,
        &stream_id,
        host_epoch,
    )
    .expect("mobile Noise IK initiator");

    let msg1 = mobile_hs.write_message(&[]).expect("mobile Noise IK msg1");
    let hs_frame_1 = OuterFrame {
        v: GPTF_VERSION,
        frame_id: format!("{stream_id}:hs1"),
        stream_id: stream_id.clone(),
        seq: 0,
        from_device: mobile_device_id.clone(),
        to_devices: vec![host_device_id.clone()],
        ttl_sec: 30,
        class: FrameClass::Control,
        ciphertext_b64: base64::engine::general_purpose::STANDARD.encode(&msg1),
    };
    mobile_ws
        .send(Message::Text(
            serde_json::to_string(&hs_frame_1).unwrap().into(),
        ))
        .await
        .expect("send Noise IK msg1");

    let host_rx_hs1_raw = recv_ws_text(&mut host_ws, "host receive Noise IK msg1").await;
    let host_rx_hs1: OuterFrame =
        serde_json::from_str(&host_rx_hs1_raw).expect("parse hs1 OuterFrame");
    assert_eq!(host_rx_hs1.from_device, mobile_device_id);
    let rx_msg1 = base64::engine::general_purpose::STANDARD
        .decode(&host_rx_hs1.ciphertext_b64)
        .expect("decode msg1");

    let msg2 = host_noise_mgr
        .accept_mobile(&host_x_secret, &mobile_cert, &host_rev_cache, &rx_msg1, now)
        .expect("host accept_mobile Noise IK");

    let hs_frame_2 = OuterFrame {
        v: GPTF_VERSION,
        frame_id: format!("{stream_id}:hs2"),
        stream_id: stream_id.clone(),
        seq: 0,
        from_device: host_device_id.clone(),
        to_devices: vec![mobile_device_id.clone()],
        ttl_sec: 30,
        class: FrameClass::Control,
        ciphertext_b64: base64::engine::general_purpose::STANDARD.encode(&msg2),
    };
    host_ws
        .send(Message::Text(
            serde_json::to_string(&hs_frame_2).unwrap().into(),
        ))
        .await
        .expect("send Noise IK msg2");

    let mobile_rx_hs2_raw = recv_ws_text(&mut mobile_ws, "mobile receive Noise IK msg2").await;
    let mobile_rx_hs2: OuterFrame =
        serde_json::from_str(&mobile_rx_hs2_raw).expect("parse hs2 OuterFrame");
    assert_eq!(mobile_rx_hs2.from_device, host_device_id);
    let rx_msg2 = base64::engine::general_purpose::STANDARD
        .decode(&mobile_rx_hs2.ciphertext_b64)
        .expect("decode msg2");

    let empty_payload = mobile_hs
        .read_message(&rx_msg2)
        .expect("mobile read Noise IK msg2");
    assert!(empty_payload.is_empty());
    assert!(mobile_hs.is_handshake_finished());
    let mut mobile_session = mobile_hs
        .into_transport()
        .expect("mobile transition to Noise transport");

    // -----------------------------------------------------------------------
    // (e) Real E2EE `inbox_snapshot` + `approval_created` (host -> relay -> mobile),
    //     zero-knowledge wire check, P-256 `SignedApprovalDecision` (`allow_once`)
    //     + `SignedSteeringConfirm` (mobile -> relay -> host), verified by live
    //     `ApprovalBroker` & `SteeringIngressGate` against fetched revocation list.
    // -----------------------------------------------------------------------
    common::run_git(&repo_dir, &["init", "-q", "-b", "main"]);
    let repo_cwd = repo_dir.to_string_lossy().into_owned();

    let key_store = Arc::new(TrustedApprovalKeyStore::new());
    key_store.set_identity_pub_ed25519(identity_pub);
    key_store
        .install_revocation_list(live_rev_list.clone(), now)
        .expect("install live revocation list into ApprovalBroker key store");
    key_store
        .enroll_device_cert(mobile_cert.clone(), now)
        .expect("enroll mobile cert into ApprovalBroker key store");

    let presence = Arc::new(MockPresenceProvider::new(UserPresence::Away));
    let broker = Arc::new(
        ApprovalBroker::new(
            host_device_id.clone(),
            mailbox_dir,
            presence,
            Arc::clone(&key_store),
        )
        .with_presence_poll_interval(Duration::from_millis(10)),
    );
    broker.set_remote_approval_active(true);

    let secret_command = "cargo test --workspace --sentinel-opaque-payload-99182";
    let tool_input = json!({ "command": secret_command });
    let broker_for_hook = Arc::clone(&broker);
    let cwd_for_hook = repo_cwd.clone();
    let input_for_hook = tool_input.clone();
    let hook_handle = std::thread::spawn(move || {
        broker_for_hook.register_and_wait_in_process(
            &RegisterApprovalRequest {
                session_id: "sess-e2e-live-1".into(),
                tool_name: "Bash".into(),
                tool_input: input_for_hook,
                cwd: cwd_for_hook,
                tool_summary: "Run workspace test suite".into(),
                wait_ms: Some(15_000),
            },
            || false,
        )
    });

    assert!(
        common::env::wait_until(Duration::from_secs(5), || {
            !broker.pending_tickets().is_empty()
        }),
        "ApprovalBroker did not register pending ticket"
    );
    let open_summary = broker.pending_tickets().into_iter().next().unwrap();

    let wire_ticket = ApprovalTicket {
        ticket_id: open_summary.ticket_id.clone(),
        host_id: open_summary.host_id.clone(),
        session_id: open_summary.session_id.clone(),
        dedup_key: open_summary.dedup_key.clone(),
        harness: "claude".into(),
        channel: ApprovalChannel::PresenceGatedHook,
        tool_name: open_summary.tool_name.clone(),
        tool_input: tool_input.clone(),
        cwd: open_summary.cwd.clone(),
        input_sha256: open_summary.input_sha256.clone(),
        summary: Some(open_summary.tool_summary.clone()),
        repo_label: Some("trusted-repo".into()),
        issued_at: open_summary.issued_at,
        expires_at: open_summary.expires_at,
    };

    let session_snap = SessionSnapshot {
        session_key: "sess-e2e-live-1".into(),
        repo_path: repo_cwd.clone(),
        repo_label: "trusted-repo".into(),
        worktree_path: repo_cwd.clone(),
        branch: Some("main".into()),
        harness: "claude".into(),
        title: "E2E Live Control Plane".into(),
        presence: "live".into(),
        fully_idle: false,
        provider_session_id: Some("claude-sess-e2e".into()),
        attention: Some("needs-you".into()),
        attention_detail: Some("Awaiting Bash approval".into()),
        capabilities: CapabilitySet {
            harness: "claude".into(),
            approval_channel: ApprovalChannel::PresenceGatedHook,
            supports_remote_approval: true,
            supports_active_steering: true,
            supports_idle_wake: false,
            ungated: false,
            queued_steering_ttl_sec: None,
            pty_input_enabled: false,
        },
        active_subagents: Vec::new(),
        pending_approval_ids: vec![wire_ticket.ticket_id.clone()],
        dirty_files: Some(1),
        updated_at_ms: now_unix_ms(),
    };

    let snapshot_evt = ControlPlaneEvent::InboxSnapshot {
        host_id: host_device_id.clone(),
        host_epoch,
        seq: 1,
        sessions: vec![session_snap],
        open_approvals: vec![wire_ticket.clone()],
        generated_at_ms: now_unix_ms(),
    };
    let created_evt = ControlPlaneEvent::ApprovalCreated {
        ticket: wire_ticket.clone(),
    };

    let snap_frames = host_noise_mgr
        .broadcast_event(FrameClass::Control, 300, &snapshot_evt)
        .expect("encrypt inbox_snapshot");
    let created_frames = host_noise_mgr
        .broadcast_event(FrameClass::Approval, 60, &created_evt)
        .expect("encrypt approval_created");
    assert_eq!(snap_frames.len(), 1);
    assert_eq!(created_frames.len(), 1);

    for frame in snap_frames.iter().chain(created_frames.iter()) {
        host_ws
            .send(Message::Text(serde_json::to_string(frame).unwrap().into()))
            .await
            .expect("send encrypted host frame");
    }

    // Receive BOTH frames on mobile WS and verify relay only saw opaque `ciphertext_b64`.
    let raw_wire_snap = recv_ws_text(&mut mobile_ws, "mobile receive inbox_snapshot").await;
    let raw_wire_created = recv_ws_text(&mut mobile_ws, "mobile receive approval_created").await;

    for (label, raw) in [
        ("inbox_snapshot", &raw_wire_snap),
        ("approval_created", &raw_wire_created),
    ] {
        let parsed: Value = serde_json::from_str(raw).expect("wire frame JSON");
        assert!(
            parsed["ciphertext_b64"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
            "{label} wire frame must carry non-empty ciphertext_b64"
        );
        for forbidden in [
            secret_command,
            "sentinel-opaque-payload-99182",
            "sess-e2e-live-1",
            "Bash",
            &repo_cwd,
            "inbox_snapshot",
            "approval_created",
        ] {
            assert!(
                !raw.contains(forbidden),
                "zero-knowledge violation: {label} wire frame leaked {forbidden:?}"
            );
        }
    }

    // Decrypt on mobile side and validate inner payloads.
    let decrypt_mobile =
        |session: &mut gitpulse_protocol::crypto::TunnelSession, raw: &str| -> ControlPlaneEvent {
            let outer: OuterFrame = serde_json::from_str(raw).expect("parse OuterFrame");
            let wire_ct = base64::engine::general_purpose::STANDARD
                .decode(&outer.ciphertext_b64)
                .expect("decode ciphertext_b64");
            let plain = session.decrypt(&wire_ct).expect("Noise decrypt on mobile");
            serde_json::from_slice(&plain).expect("parse inner ControlPlaneEvent")
        };

    let dec_snap = decrypt_mobile(&mut mobile_session, &raw_wire_snap);
    match dec_snap {
        ControlPlaneEvent::InboxSnapshot {
            host_id,
            open_approvals,
            sessions,
            ..
        } => {
            assert_eq!(host_id, host_device_id);
            assert_eq!(sessions.len(), 1);
            assert_eq!(open_approvals.len(), 1);
            assert_eq!(open_approvals[0].ticket_id, wire_ticket.ticket_id);
            assert_eq!(open_approvals[0].input_sha256, wire_ticket.input_sha256);
        }
        other => panic!("expected InboxSnapshot, got {other:?}"),
    }

    let dec_created = decrypt_mobile(&mut mobile_session, &raw_wire_created);
    let rx_ticket = match dec_created {
        ControlPlaneEvent::ApprovalCreated { ticket } => ticket,
        other => panic!("expected ApprovalCreated, got {other:?}"),
    };
    assert_eq!(rx_ticket.ticket_id, wire_ticket.ticket_id);
    assert_eq!(rx_ticket.input_sha256, wire_ticket.input_sha256);

    // Mobile signs P-256 `SignedApprovalDecision` (`allow_once`) and `SignedSteeringConfirm`.
    let decision_now = now_unix_secs();
    let signed_decision = sign_approval_decision_software(
        &mobile_p256_secret,
        &mobile_device_id,
        &host_device_id,
        &rx_ticket.ticket_id,
        &rx_ticket.input_sha256,
        ApprovalDecisionKind::AllowOnce,
        "nonce-e2e-approval-001",
        decision_now - 5,
        decision_now + 120,
    )
    .expect("sign P-256 approval decision");

    let steering_text = "Focus strictly on e2e-sentinel-steering-text-7741";
    let steering_req_id = "steer-e2e-req-1";
    let steering_route = "mailbox:active_turn";
    let signed_steering = sign_steering_confirm_software(
        &mobile_p256_secret,
        &mobile_device_id,
        &host_device_id,
        "sess-e2e-live-1",
        steering_req_id,
        steering_route,
        steering_text,
        "nonce-e2e-steering-001",
        decision_now - 5,
        decision_now + 120,
    )
    .expect("sign P-256 steering confirm");

    let send_encrypted_from_mobile = |session: &mut gitpulse_protocol::crypto::TunnelSession,
                                      class: FrameClass,
                                      ttl_sec: u32,
                                      evt: &ControlPlaneEvent|
     -> OuterFrame {
        let plain = serde_json::to_vec(evt).unwrap();
        let wire_ct = session.encrypt(&plain).expect("mobile Noise encrypt");
        let seq = session.send_seq();
        OuterFrame {
            v: GPTF_VERSION,
            frame_id: format!("{mobile_device_id}:{seq}"),
            stream_id: stream_id.clone(),
            seq,
            from_device: mobile_device_id.clone(),
            to_devices: vec![host_device_id.clone()],
            ttl_sec,
            class,
            ciphertext_b64: base64::engine::general_purpose::STANDARD.encode(&wire_ct),
        }
    };

    let approval_outer = send_encrypted_from_mobile(
        &mut mobile_session,
        FrameClass::Approval,
        60,
        &ControlPlaneEvent::ApprovalDecide {
            decision: signed_decision.clone(),
        },
    );
    let steering_outer = send_encrypted_from_mobile(
        &mut mobile_session,
        FrameClass::Control,
        60,
        &ControlPlaneEvent::SteeringConfirmReq {
            req: SteeringConfirmReq {
                request_id: steering_req_id.into(),
                session_key: "sess-e2e-live-1".into(),
                expect_route: steering_route.into(),
                text: steering_text.into(),
                confirm_token: None,
                signed: Some(signed_steering.clone()),
            },
        },
    );

    mobile_ws
        .send(Message::Text(
            serde_json::to_string(&approval_outer).unwrap().into(),
        ))
        .await
        .expect("send ApprovalDecide");
    mobile_ws
        .send(Message::Text(
            serde_json::to_string(&steering_outer).unwrap().into(),
        ))
        .await
        .expect("send SteeringConfirmReq");

    // Host receives BOTH encrypted frames from live relay WS, checks zero plaintext leak,
    // decrypts via `HostNoiseManager::open_from_mobile`, and verifies with `ApprovalBroker`
    // and `SteeringIngressGate` against the live revocation list.
    let raw_host_rx_approval = recv_ws_text(&mut host_ws, "host receive ApprovalDecide").await;
    let raw_host_rx_steering = recv_ws_text(&mut host_ws, "host receive SteeringConfirmReq").await;

    for forbidden in [
        "allow_once",
        "approval_decide",
        "nonce-e2e-approval-001",
        steering_text,
        "e2e-sentinel-steering-text-7741",
        "steering_confirm_req",
    ] {
        assert!(
            !raw_host_rx_approval.contains(forbidden) && !raw_host_rx_steering.contains(forbidden),
            "zero-knowledge violation on mobile->host frame: leaked {forbidden:?}"
        );
    }

    let rx_approval_outer: OuterFrame =
        serde_json::from_str(&raw_host_rx_approval).expect("parse approval OuterFrame");
    let rx_approval_evt = host_noise_mgr
        .open_from_mobile(&rx_approval_outer, &host_rev_cache)
        .expect("host decrypt ApprovalDecide");
    let rx_decision = match rx_approval_evt {
        ControlPlaneEvent::ApprovalDecide { decision } => decision,
        other => panic!("expected ApprovalDecide, got {other:?}"),
    };

    let outcome = broker
        .resolve_signed(&rx_decision)
        .expect("ApprovalBroker::resolve_signed with live revocation list");
    assert_eq!(outcome.status, ResolveStatus::Applied);
    assert_eq!(
        outcome.applied_decision,
        Some(ApprovalDecisionKind::AllowOnce)
    );

    let hook_reply = hook_handle.join().expect("join waiting hook thread");
    assert_eq!(
        hook_reply,
        HookBrokerReply::Decision {
            ticket_id: wire_ticket.ticket_id.clone(),
            decision: ApprovalDecisionKind::AllowOnce,
            reason: None,
        }
    );

    // Replaying the exact same SignedApprovalDecision must be rejected.
    assert_eq!(
        broker.resolve_signed(&rx_decision),
        Err(BrokerError::NonceReplayed)
    );

    let rx_steering_outer: OuterFrame =
        serde_json::from_str(&raw_host_rx_steering).expect("parse steering OuterFrame");
    let rx_steering_evt = host_noise_mgr
        .open_from_mobile(&rx_steering_outer, &host_rev_cache)
        .expect("host decrypt SteeringConfirmReq");
    let rx_steering_req = match rx_steering_evt {
        ControlPlaneEvent::SteeringConfirmReq { req } => req,
        other => panic!("expected SteeringConfirmReq, got {other:?}"),
    };

    let steering_gate = SteeringIngressGate::new(&host_device_id);
    let steering_verdict = steering_gate
        .verify_strict(
            rx_steering_req.signed.as_ref().expect("signed steering"),
            &rx_steering_req.text,
            &rx_steering_req.session_key,
            &rx_steering_req.request_id,
            &rx_steering_req.expect_route,
            &mobile_cert,
            &host_rev_cache,
            now_unix_secs(),
        )
        .expect("SteeringIngressGate::verify_strict with live revocation list");
    assert_eq!(
        steering_verdict,
        SteeringIngressVerdict::HumanConfirmed {
            device_id: mobile_device_id.clone(),
            request_id: steering_req_id.into(),
            text_sha256: steering_text_sha256_hex(steering_text),
        }
    );
    // Replaying the exact same SignedSteeringConfirm must fail closed.
    assert!(matches!(
        steering_gate.verify_strict(
            rx_steering_req.signed.as_ref().unwrap(),
            &rx_steering_req.text,
            &rx_steering_req.session_key,
            &rx_steering_req.request_id,
            &rx_steering_req.expect_route,
            &mobile_cert,
            &host_rev_cache,
            now_unix_secs(),
        ),
        Err(ProtocolError::ReplayDetected { .. })
    ));

    // -----------------------------------------------------------------------
    // (f) Call `DELETE /v1/devices/{mobile_id}` (`RevokeDevice`) on the live
    //     relay and assert mobile WS receives immediate close code `4003`.
    // -----------------------------------------------------------------------
    let revoke_resp = http_request(
        &relay.addr,
        "DELETE",
        &format!("/v1/devices/{mobile_device_id}"),
        &bearer,
        &[],
    )
    .await;
    assert_eq!(
        revoke_resp.status,
        204,
        "DELETE /v1/devices/{mobile_device_id}: {}",
        revoke_resp.text()
    );

    let close_limit = common::env::coverage_relaxed(Duration::from_secs(5));
    let observed_close_code = tokio::time::timeout(close_limit, async {
        loop {
            match mobile_ws.next().await {
                Some(Ok(Message::Close(Some(frame)))) => break u16::from(frame.code),
                Some(Ok(Message::Ping(payload))) => {
                    let _ = mobile_ws.send(Message::Pong(payload)).await;
                }
                Some(Ok(Message::Pong(_))) => continue,
                other => panic!(
                    "expected WS Close(4003) on revoked mobile tunnel, got {other:?}\nrelay logs:\n{}",
                    relay.dump_logs()
                ),
            }
        }
    })
    .await
    .expect("timed out waiting for immediate WS close 4003 after DELETE /v1/devices/{id}");
    assert_eq!(
        observed_close_code, WS_CLOSE_REVOKED_OR_DOWNGRADED,
        "expected WS close code 4003 on device revocation"
    );

    // Publish and fetch SignedRevocationList v2 containing mobile_device_id,
    // and assert both HostNoiseManager and ApprovalBroker reject it.
    let rev_now = now_unix_secs();
    let rev_v2 = issue_revocation_list(
        &identity_seed,
        2,
        rev_now - 10,
        rev_now + 3600,
        vec![mobile_device_id.clone()],
    )
    .expect("issue SignedRevocationList v2");
    let put_rev_v2 = signed_http_request(
        &relay.addr,
        &clock,
        &host_device_id,
        &host_ed_seed,
        "PUT",
        "/v1/identity/revocations",
        &serde_json::to_vec(&rev_v2).unwrap(),
    )
    .await;
    assert_eq!(
        put_rev_v2.status,
        200,
        "PUT /v1/identity/revocations v2: {}",
        put_rev_v2.text()
    );

    let get_rev_v2 = signed_http_request(
        &relay.addr,
        &clock,
        &host_device_id,
        &host_ed_seed,
        "GET",
        "/v1/identity/revocations",
        &[],
    )
    .await;
    assert_eq!(get_rev_v2.status, 200);
    let live_rev_v2: SignedRevocationList =
        serde_json::from_slice(&get_rev_v2.body).expect("parse live rev v2");
    assert_eq!(live_rev_v2.version, 2);
    assert_eq!(
        live_rev_v2.revoked_device_ids,
        vec![mobile_device_id.clone()]
    );

    host_rev_cache
        .update(live_rev_v2.clone(), rev_now)
        .expect("update host revocation cache to v2");
    assert_eq!(
        host_noise_mgr.evict_revoked(&host_rev_cache),
        vec![mobile_device_id.clone()]
    );
    key_store
        .install_revocation_list(live_rev_v2, rev_now)
        .expect("install v2 revocation list in ApprovalBroker");
    let fresh_after_revoke = sign_approval_decision_software(
        &mobile_p256_secret,
        &mobile_device_id,
        &host_device_id,
        &rx_ticket.ticket_id,
        &rx_ticket.input_sha256,
        ApprovalDecisionKind::AllowOnce,
        "nonce-e2e-after-revoke-002",
        rev_now - 5,
        rev_now + 120,
    )
    .expect("sign decision after revoke");
    assert_eq!(
        broker.resolve_signed(&fresh_after_revoke),
        Err(BrokerError::DeviceRevoked(mobile_device_id.clone()))
    );

    eprintln!(
        "[remote_relay_e2e] PASS live relay={} relay_dir={} host={} mobile={} close_code={}",
        relay.addr,
        relay_dir.display(),
        host_device_id,
        mobile_device_id,
        observed_close_code
    );

    let _ = host_ws.close(None).await;
    gh_task.abort();
}
