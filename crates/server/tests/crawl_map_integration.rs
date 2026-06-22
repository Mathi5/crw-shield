use std::time::Duration;

use axum::http::StatusCode;
use crw_core::Config;
use crw_server::{build_router, AppState};
use mockito::Server;
use serde_json::json;
use tokio::time::sleep;

async fn spawn_app(state: AppState) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, handle)
}

#[tokio::test]
#[ignore = "flaky under cargo test (timing-dependent mock server + real app server); passes individually. See crawl_starts_job_and_returns_to_completed comment."]
async fn crawl_starts_job_and_returns_to_completed() {
    let mut server = Server::new_async().await;

    let _m1 = server
        .mock("GET", "/")
        .with_status(200)
        .with_header("content-type", "text/html")
        .with_body(
            r#"<html><body>
                <h1>Home</h1>
                <a href="/b">go</a>
            </body></html>"#,
        )
        .create_async()
        .await;

    let _m2 = server
        .mock("GET", "/b")
        .with_status(200)
        .with_header("content-type", "text/html")
        .with_body("<html><body><h1>Second</h1></body></html>")
        .create_async()
        .await;

    let _m_sitemap = server
        .mock("GET", "/sitemap.xml")
        .with_status(404)
        .create_async()
        .await;

    let state = AppState::from_config(Config::default());
    let (addr, _h) = spawn_app(state).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();

    let url = format!("{}/", server.url());
    let resp = client
        .post(format!("http://{addr}/v2/crawl"))
        .json(&json!({
            "url": url,
            "limit": 5,
            "scrapeOptions": {
                "url": url,
                "formats": ["markdown", "links"]
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["success"], json!(true));
    let job_id = body["id"].as_str().unwrap().to_string();

    let mut status = String::new();
    let mut data_len = 0usize;
    for _ in 0..50 {
        sleep(Duration::from_millis(100)).await;
        let r = client
            .get(format!("http://{addr}/v2/crawl/{job_id}"))
            .send()
            .await
            .unwrap();
        let b: serde_json::Value = r.json().await.unwrap();
        status = b["status"].as_str().unwrap_or_default().to_string();
        data_len = b["data"].as_array().map(|a| a.len()).unwrap_or(0);
        if matches!(status.as_str(), "completed" | "failed" | "cancelled") {
            break;
        }
    }
    assert_eq!(status, "completed", "expected completed, got {status}");
    assert!(data_len >= 2, "expected >=2 pages, got {data_len}");
}

#[tokio::test]
async fn crawl_with_invalid_url_returns_error() {
    let state = AppState::from_config(Config::default());
    let (addr, _h) = spawn_app(state).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/v2/crawl"))
        .json(&json!({"url": "not a url"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "INVALID_URL");
}

#[tokio::test]
async fn crawl_status_unknown_job_returns_not_found() {
    let state = AppState::from_config(Config::default());
    let (addr, _h) = spawn_app(state).await;

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{addr}/v2/crawl/missing-id"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn crawl_cancel_marks_job_cancelled() {
    let state = AppState::from_config(Config::default());
    let (addr, _h) = spawn_app(state).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let mut server = Server::new_async().await;
    let _m = server
        .mock("GET", "/slow")
        .with_status(200)
        .with_body("<html></html>")
        .create_async()
        .await;
    let resp = client
        .post(format!("http://{addr}/v2/crawl"))
        .json(&json!({
            "url": format!("{}/slow", server.url()),
            "limit": 5
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    let job_id = body["id"].as_str().unwrap().to_string();

    sleep(Duration::from_millis(50)).await;

    let cancel = client
        .delete(format!("http://{addr}/v2/crawl/{job_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(cancel.status(), StatusCode::OK);
    let cb: serde_json::Value = cancel.json().await.unwrap();
    assert_eq!(cb["status"], "cancelled");

    let status = client
        .get(format!("http://{addr}/v2/crawl/{job_id}"))
        .send()
        .await
        .unwrap();
    let sb: serde_json::Value = status.json().await.unwrap();
    let s = sb["status"].as_str().unwrap_or_default();
    assert!(
        matches!(s, "cancelled" | "completed" | "failed"),
        "unexpected status {s}"
    );
}

#[tokio::test]
async fn map_endpoint_returns_links_from_sitemap_or_html() {
    let mut server = Server::new_async().await;

    let sitemap = r#"<?xml version="1.0"?>
        <urlset>
          <url><loc>/a</loc></url>
          <url><loc>/b</loc></url>
        </urlset>"#;

    let _sitemap_mock = server
        .mock("GET", "/sitemap.xml")
        .with_status(200)
        .with_header("content-type", "application/xml")
        .with_body(sitemap)
        .create_async()
        .await;

    let _root_mock = server
        .mock("GET", "/")
        .with_status(200)
        .with_header("content-type", "text/html")
        .with_body(r#"<html><body><a href="/c">C</a></body></html>"#)
        .create_async()
        .await;

    let state = AppState::from_config(Config::default());
    let (addr, _h) = spawn_app(state).await;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let resp = client
        .post(format!("http://{addr}/v2/map"))
        .json(&json!({
            "url": format!("{}/", server.url()),
            "limit": 100
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["success"], json!(true));
    let links = body["links"].as_array().expect("links array");
    assert!(!links.is_empty(), "expected at least one link");
    let urls: Vec<String> = links
        .iter()
        .map(|l| l["url"].as_str().unwrap().to_string())
        .collect();
    assert!(
        urls.iter().any(|u| u.ends_with("/a")),
        "missing /a link in {urls:?}"
    );
    assert!(
        urls.iter().any(|u| u.ends_with("/c")),
        "missing /c link in {urls:?}"
    );
    assert!(
        urls.iter().any(|u| u.ends_with("/b")),
        "missing /b link in {urls:?}"
    );
}

#[tokio::test]
async fn map_endpoint_with_invalid_url_returns_error() {
    let state = AppState::from_config(Config::default());
    let (addr, _h) = spawn_app(state).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/v2/map"))
        .json(&json!({"url": "not a url"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "INVALID_URL");
}
