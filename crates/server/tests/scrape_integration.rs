use std::time::Duration;

use axum::http::StatusCode;
use crw_core::{Config, ScrapeRequest};
use crw_server::{build_router, AppState};
use mockito::Server;
use serde_json::json;

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

    let state = AppState::from_config(Config::default());
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
    let state = AppState::from_config(Config::default());
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
        .with_body("<html><body>x</body></html>")
        .create_async()
        .await;

    let url = format!("{}/p", server.url());
    let cfg = Config {
        auth_token: Some("secret-token".to_string()),
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
