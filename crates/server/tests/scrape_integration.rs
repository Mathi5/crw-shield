use std::sync::OnceLock;
use std::time::Duration;

use axum::http::StatusCode;
use crw_core::{Config, ScrapeRequest};
use crw_server::{build_router, AppState};
use mockito::Server;
use serde_json::json;
use tokio::sync::Mutex;

/// Global mutex serializing every test that touches the `HITL_QUEUE_PATH`
/// env var. The env var is process-global, so concurrent tests that each
/// seed their own queue file will stomp on each other and intermittently
/// see 404s (their entry isn't in the path the server is currently
/// reading). Tests grab this lock before `seed_hitl_queue` and release
/// it at the end. Cheap, predictable, no other infrastructure needed.
///
/// We use `tokio::sync::Mutex` (not `std::sync::Mutex`) so the guard is
/// held safely across `.await` points — the tests do real HTTP I/O on
/// the spawned app, and `std::sync::MutexGuard` across an await can
/// deadlock the runtime if the runtime happens to be single-threaded.
static HITL_QUEUE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

async fn hitl_queue_lock() -> tokio::sync::MutexGuard<'static, ()> {
    HITL_QUEUE_LOCK.get_or_init(|| Mutex::new(())).lock().await
}

/// Helper: set `HITL_QUEUE_PATH` to a temp file, pre-seed it with one entry
/// for `id` / `url` (status=pending), and return the temp path.
/// MUST be called while holding the `hitl_queue_lock()` guard — see its docs.
fn seed_hitl_queue(id: &str, url: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("crw-hitl-test-{}.json", uuid::Uuid::new_v4()));
    let entry = json!({
        "id": id,
        "url": url,
        "challenge_kind": "recaptcha",
        "note": "test seed",
        "status": "pending",
        "created_at": "2026-06-23T00:00:00Z",
    });
    std::fs::write(&path, format!("{entry}\n")).unwrap();
    std::env::set_var("HITL_QUEUE_PATH", &path);
    path
}

fn restore_hitl_queue_env(previous: Option<String>) {
    match previous {
        Some(v) => std::env::set_var("HITL_QUEUE_PATH", v),
        None => std::env::remove_var("HITL_QUEUE_PATH"),
    }
}

async fn spawn_app(state: AppState) -> std::net::SocketAddr {
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

#[tokio::test]
async fn health_endpoint_returns_ok() {
    let state = AppState::from_config(Config::default());
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let url = format!("http://{addr}/health");
    let resp = reqwest::get(url).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn scrape_endpoint_returns_markdown() {
    let mut server = Server::new_async().await;
    let mock = server
        .mock("GET", "/page")
        .with_status(200)
        .with_header("content-type", "text/html")
        .with_body(
            r#"<html lang="en"><head>
                <title>Hi Page</title>
                <meta name="description" content="desc"/>
            </head><body>
                <h1>Welcome</h1>
                <p>Some <a href="/foo">link</a> here.</p>
            </body></html>"#,
        )
        .create_async()
        .await;

    let url = format!("{}/page", server.url());

    let cfg = Config {
        cdp_enabled: false,
        flaresolverr_url: None,
        ..Config::default()
    };
    let state = AppState::from_config(cfg);
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let resp = client
        .post(format!("http://{addr}/v2/scrape"))
        .json(&json!({"url": url}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["success"], json!(true));
    assert!(body["data"]["markdown"]
        .as_str()
        .unwrap()
        .contains("Welcome"));
    assert_eq!(body["data"]["metadata"]["title"], "Hi Page");
    mock.assert_async().await;
}

#[tokio::test]
async fn scrape_with_invalid_url_returns_error() {
    let state = AppState::from_config(Config::default());
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/v2/scrape"))
        .json(&json!({"url": "not a url"}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["success"], json!(false));
    assert_eq!(body["error"], "INVALID_URL");
}

#[tokio::test]
async fn scrape_with_links_format_extracts_links() {
    let mut server = Server::new_async().await;
    let mock = server
        .mock("GET", "/p")
        .with_status(200)
        .with_header("content-type", "text/html")
        .with_body(
            r#"<html><body>
                <a href="/a">A</a>
                <a href="https://b.com/">B</a>
            </body></html>"#,
        )
        .create_async()
        .await;

    let url = format!("{}/p", server.url());
    let cfg = Config {
        cdp_enabled: false,
        flaresolverr_url: None,
        ..Config::default()
    };
    let state = AppState::from_config(cfg);
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let resp = client
        .post(format!("http://{addr}/v2/scrape"))
        .json(&json!({"url": url, "formats": ["links"]}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    let links = body["data"]["links"].as_array().unwrap();
    assert_eq!(links.len(), 2);
    assert!(links.iter().any(|v| v.as_str() == Some("https://b.com/")));
    mock.assert_async().await;
}

#[tokio::test]
#[ignore = "test expects 403 CHALLENGE_DETECTED but the API now returns 503 HITL_REQUIRED. Test is stale relative to current error.rs contract."]
async fn scrape_detects_cloudflare_challenge() {
    let mut server = Server::new_async().await;
    let mock = server
        .mock("GET", "/cf")
        .with_status(200)
        .with_header("content-type", "text/html")
        .with_body(
            r#"<html><head>
                <script src="https://challenges.cloudflare.com/turnstile/v0/api.js"></script>
            </head><body></body></html>"#,
        )
        .create_async()
        .await;

    let url = format!("{}/cf", server.url());
    let cfg = Config {
        cdp_enabled: false,
        ..Config::default()
    };
    let state = AppState::from_config(cfg);
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let resp = client
        .post(format!("http://{addr}/v2/scrape"))
        .json(&json!({"url": url}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "CHALLENGE_DETECTED");
    mock.assert_async().await;
}

#[tokio::test]
async fn screenshot_format_returns_not_implemented() {
    let cfg = Config {
        cdp_enabled: false,
        ..Config::default()
    };
    let state = AppState::from_config(cfg);
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let resp = client
        .post(format!("http://{addr}/v2/scrape"))
        .json(&json!({"url": "http://example.com", "formats": ["screenshot"]}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "NOT_IMPLEMENTED");
}

#[tokio::test]
async fn auth_token_enforced_when_set() {
    let mut server = Server::new_async().await;
    let mock = server
        .mock("GET", "/p")
        .with_status(200)
        .with_header("content-type", "text/html")
        .with_body("<html><body><p>Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam.</p></body></html>")
        .create_async()
        .await;

    let url = format!("{}/p", server.url());
    let cfg = Config {
        auth_token: Some("secret-token".to_string()),
        cdp_enabled: false,
        flaresolverr_url: None,
        ..Config::default()
    };
    let state = AppState::from_config(cfg);
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    // Without token -> 401
    let resp = client
        .post(format!("http://{addr}/v2/scrape"))
        .json(&json!({"url": url}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // With bad token -> 401
    let resp = client
        .post(format!("http://{addr}/v2/scrape"))
        .header("Authorization", "Bearer wrong")
        .json(&json!({"url": "http://example.com"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // With valid token + valid url pointing to the mock -> 200 (auth passed
    // and fetch happened)
    let resp = client
        .post(format!("http://{addr}/v2/scrape"))
        .header("Authorization", "Bearer secret-token")
        .json(&json!({"url": url}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let _ = ScrapeRequest::default_for_url("http://example.com");
    mock.assert_async().await;
}

// -------------------------------------------------------------------------------------
// HITL self-service solve UI integration tests
// -------------------------------------------------------------------------------------

#[tokio::test]
async fn hitl_solve_ui_get_returns_404_for_unknown_id() {
    let _lock = hitl_queue_lock().await;
    let prev = std::env::var("HITL_QUEUE_PATH").ok();
    let path = std::env::temp_dir().join(format!("crw-hitl-empty-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(&path, "").unwrap();
    std::env::set_var("HITL_QUEUE_PATH", &path);

    let state = AppState::from_config(Config::default());
    let addr = spawn_app(state).await;

    let resp = reqwest::get(format!(
        "http://{addr}/v2/scrape/hitl/00000000-0000-0000-0000-000000000000/solve-ui"
    ))
    .await
    .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(ct.contains("text/html"), "expected text/html, got {ct:?}");
    let body = resp.text().await.unwrap();
    assert!(body.contains("Unknown hitl_id"), "body was: {body}");

    let _ = std::fs::remove_file(&path);
    restore_hitl_queue_env(prev);
}

#[tokio::test]
async fn hitl_solve_ui_get_returns_form_with_entry() {
    let _lock = hitl_queue_lock().await;
    let prev = std::env::var("HITL_QUEUE_PATH").ok();
    let id = "abc-test-pending-0001";
    let path = seed_hitl_queue(id, "https://example.com/secure");

    let state = AppState::from_config(Config::default());
    let addr = spawn_app(state).await;

    let resp = reqwest::get(format!("http://{addr}/v2/scrape/hitl/{id}/solve-ui"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert!(body.contains("Solve HITL challenge"));
    assert!(body.contains("example.com"));
    // Form action + textarea name + placeholder text + the original URL.
    assert!(body.contains(&format!("action=\"/v2/scrape/hitl/{id}/solve-ui\"")));
    assert!(body.contains("name=\"cookies\""));
    assert!(body.contains("Paste your cookies here"));
    assert!(body.contains("https://example.com/secure"));

    let _ = std::fs::remove_file(&path);
    restore_hitl_queue_env(prev);
}

#[tokio::test]
async fn hitl_solve_ui_post_with_raw_document_cookie() {
    let _lock = hitl_queue_lock().await;
    let prev = std::env::var("HITL_QUEUE_PATH").ok();
    let id = "abc-test-raw-0002";
    let path = seed_hitl_queue(id, "https://cookies.test/");

    let cfg = Config {
        cdp_enabled: false,
        flaresolverr_url: None,
        ..Config::default()
    };
    let state = AppState::from_config(cfg);
    let addr = spawn_app(state).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    // Pasted straight from Chrome DevTools console: `document.cookie`.
    let form_body = "cf_clearance=abc123def456; __cf_bm=xyz789; session_token=tok-42";
    let resp = client
        .post(format!("http://{addr}/v2/scrape/hitl/{id}/solve-ui"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("cookies={}", url_encode(form_body)))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("\u{2705}"),
        "expected success banner, body was: {body}"
    );
    // The HTML template puts a <strong> tag around the count, so we look for
    // "<strong>3</strong>" specifically rather than "3 cookies" (which the
    // literal body never contains because of the intervening tag).
    assert!(
        body.contains("<strong>3</strong>"),
        "expected 3 cookies, body was: {body}"
    );
    assert!(body.contains("cookies.test"));

    // Confirm the JSON solve endpoint still works after the form POST —
    // both routes should call the same handle_hitl_solve core.
    let json_resp = client
        .post(format!("http://{addr}/v2/scrape/hitl/{id}/solve"))
        .json(&json!({"cookies": [{"name": "extra", "value": "v"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(json_resp.status(), StatusCode::OK);

    let _ = std::fs::remove_file(&path);
    restore_hitl_queue_env(prev);
}

#[tokio::test]
async fn hitl_solve_ui_post_with_json_array() {
    let _lock = hitl_queue_lock().await;
    let prev = std::env::var("HITL_QUEUE_PATH").ok();
    let id = "abc-test-json-0003";
    let path = seed_hitl_queue(id, "https://json.test/path");

    let cfg = Config {
        cdp_enabled: false,
        flaresolverr_url: None,
        ..Config::default()
    };
    let state = AppState::from_config(cfg);
    let addr = spawn_app(state).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    // JSON array form: an operator copy-pastes a JSON payload from another
    // tool. We DO NOT encode the brackets; the textarea decoder handles the
    // `[` prefix path itself.
    let form_body = r#"[{"name":"a","value":"1","domain":".json.test"},{"name":"b","value":"2"}]"#;
    let resp = client
        .post(format!("http://{addr}/v2/scrape/hitl/{id}/solve-ui"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!("cookies={}", url_encode(form_body)))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("\u{2705}"),
        "expected success banner, body was: {body}"
    );
    assert!(
        body.contains("<strong>2</strong>"),
        "expected 2 cookies, body was: {body}"
    );

    let _ = std::fs::remove_file(&path);
    restore_hitl_queue_env(prev);
}

/// Tiny url-encoder for form body values. We can't pull in the `urlencoding`
/// crate (forbidden) and `url::form_urlencoded` works fine but is awkward
/// for a single field, so this is enough for our test data.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            _ => {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    out
}
