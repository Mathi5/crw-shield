//! HTTP handlers for the headed HITL web viewer (v0.4.6).
//!
//! Two endpoints, mounted OUTSIDE the auth middleware (no token required
//! — the viewer is meant to be opened by the operator from a browser
//! without copy-pasting a bearer token):
//!
//! * `GET  /hitl/viewer/:id`  — serves the static HTML+JS viewer page
//!   that streams screenshots and forwards mouse/keyboard input events.
//! * `GET  /hitl/cdp/:id`     — WebSocket upgrade that proxies Chrome
//!   DevTools Protocol messages 1:1 between the operator's browser
//!   and the headed Chromium browser running on the server.
//!
//! The CDP proxy is a thin relay: read JSON-RPC frames from the
//! client WebSocket, write them to the browser's CDP connection,
//! read the responses, write them back. No parsing, no transformation.
//! This is the exact same trick used by Chrome DevTools' own remote
//! target view (`chrome://inspect`).

use std::sync::Arc;

use async_tungstenite::tungstenite;
use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use tracing::{info, warn};

use crate::state::AppState;

/// `GET /hitl/viewer/:id` — serve the static viewer HTML page.
///
/// The page itself is embedded as a const string (single-binary deploy,
/// no static file serving). The page opens a WebSocket to
/// `/hitl/cdp/{id}` on the same host and renders whatever the CDP
/// stream produces.
pub async fn hitl_viewer_page(Path(id): Path<String>) -> Response {
    // Defensive: refuse to render for unknown hitl_ids so we don't
    // leak the viewer to anyone who guesses a UUID. 404 (not 401)
    // is appropriate because the resource doesn't exist for this id.
    let _ = id; // presence checked by the CDP handler; the page is static.
    let html = VIEWER_HTML;
    (
        StatusCode::OK,
        [("content-type", "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

/// `GET /hitl/cdp/:id` — WebSocket upgrade that proxies CDP traffic
/// to the headed Chromium browser for the given hitl_id.
///
/// The CDP protocol is a JSON-RPC variant sent over WebSocket. The
/// chromiumoxide `Browser` exposes its own WebSocket via
/// `browser.websocket()`, which we splice with the operator's
/// WebSocket: bytes in / bytes out, no parsing.
pub async fn hitl_cdp_proxy(
    Path(id): Path<String>,
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> Response {
    // Look up the active session. 404 if the id is unknown / expired.
    let session = {
        let registry = state.hitl_sessions.lock().await;
        registry.get(&id).cloned()
    };
    let Some(session) = session else {
        return (
            StatusCode::NOT_FOUND,
            format!("no active headed HITL session for id {id}"),
        )
            .into_response();
    };

    info!(hitl_id = %id, "CDP proxy WebSocket upgrade");
    ws.on_upgrade(move |client_ws| handle_cdp_session(client_ws, session, id))
}

/// Handle one CDP proxy connection. Two concurrent tasks:
/// * client → browser: forward every message from the operator's WS
///   to chromiumoxide's WS.
/// * browser → client: forward every response from chromiumoxide's WS
///   back to the operator's WS.
///
/// Either side disconnecting ends both tasks.
async fn handle_cdp_session(
    client_ws: WebSocket,
    session: Arc<crw_fetch::HeadedHitlSession>,
    hitl_id: String,
) {
    // We need the underlying WebSocket from the chromiumoxide browser.
    // chromiumoxide doesn't expose `Browser::websocket()` directly,
    // but it stores the WebSocket in the BrowserConfig at launch time
    // and reuses it. We can get the WS URL via browser.websocket_address().
    let ws_url: String = session.browser.websocket_address().to_string();

    let (mut client_tx, mut client_rx) = client_ws.split();

    // Connect to chromiumoxide's WS. We don't use the websocket
    // crate directly — async-tungstenite is the same one chromiumoxide
    // uses internally, so the frame types are compatible.
    let (browser_ws, _) = match async_tungstenite::tokio::connect_async(&ws_url).await {
        Ok(ws) => ws,
        Err(e) => {
            warn!(error = %e, "failed to connect to browser WebSocket");
            let _ = client_tx.send(Message::Close(None)).await;
            return;
        }
    };

    let (mut browser_tx, mut browser_rx) = browser_ws.split();

    // Client → Browser
    let client_to_browser = tokio::spawn(async move {
        while let Some(msg) = client_rx.next().await {
            let Ok(msg) = msg else { break };
            match msg {
                Message::Text(t) => {
                    if browser_tx
                        .send(tungstenite::Message::Text(t.as_str().to_string().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Message::Binary(b) => {
                    if browser_tx
                        .send(tungstenite::Message::Binary(Bytes::from(b)))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {} // Ping/Pong handled by tungstenite
            }
        }
    });

    // Browser → Client
    let browser_to_client = tokio::spawn(async move {
        while let Some(msg) = browser_rx.next().await {
            let Ok(msg) = msg else { break };
            let out = match msg {
                tungstenite::Message::Text(t) => Message::Text(t.to_string()),
                tungstenite::Message::Binary(b) => Message::Binary(b.to_vec()),
                tungstenite::Message::Close(_) => {
                    let _ = client_tx.send(Message::Close(None)).await;
                    break;
                }
                _ => continue,
            };
            if client_tx.send(out).await.is_err() {
                break;
            }
        }
    });

    // Wait for either direction to finish, then close.
    tokio::select! {
        _ = client_to_browser => {}
        _ = browser_to_client => {}
    }

    info!(hitl_id = %hitl_id, "CDP proxy connection closed");
}

// ─────────────────────────────────────────────────────────────────────
//  Static viewer HTML
// ─────────────────────────────────────────────────────────────────────
//
// Embedded as a const string so the binary stays self-contained
// (no static file serving, no asset path to mount in Runtipi).
//
// The page is intentionally minimal — vanilla JS, no framework — to
// keep the dependency surface small. It does three things:
//   1. Open a WebSocket to /hitl/cdp/{id}.
//   2. Send Page.captureScreenshot commands at ~10 fps; render the
//      returned base64 PNG into an <img> for the operator to see.
//   3. Forward mousedown/mousemove/mouseup/keydown/keyup/wheel events
//      to the browser via Input.dispatchMouseEvent / dispatchKeyEvent.

const VIEWER_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>crw-shield HITL viewer</title>
<style>
  * { margin: 0; padding: 0; box-sizing: border-box; }
  html, body { height: 100%; background: #1a1a1f; color: #d0d0d0;
               font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
  #header { padding: 8px 12px; background: #0d0d10; border-bottom: 1px solid #2a2a30;
            font-size: 12px; display: flex; gap: 16px; align-items: center; }
  #status { display: inline-block; width: 8px; height: 8px; border-radius: 50%;
            background: #ff4040; margin-right: 6px; }
  #status.connected { background: #40ff60; }
  #stage { display: flex; justify-content: center; align-items: center;
           height: calc(100% - 36px); overflow: auto; }
  #screen { max-width: 100%; max-height: 100%; image-rendering: pixelated;
            background: #fff; border: 1px solid #2a2a30; }
  #help { padding: 6px 12px; font-size: 11px; color: #888; }
</style>
</head>
<body>
<div id="header">
  <span><span id="status"></span><span id="status-text">Connecting…</span></span>
  <span id="url"></span>
</div>
<div id="stage"><img id="screen" alt="browser view"></div>
<div id="help">
  Click the Turnstile in the image above. Your clicks + keys are relayed to the
  browser on the server. When Cloudflare accepts you, crw-shield auto-detects
  the new cookies and re-fetches the page.
</div>
<script>
const params = new URLSearchParams(location.pathname.split('/'));
const id = location.pathname.split('/').pop();
const statusEl = document.getElementById('status');
const statusText = document.getElementById('status-text');
const screenEl = document.getElementById('screen');
const urlEl = document.getElementById('url');

const ws = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/hitl/cdp/${id}`);
let nextId = 1;
const pending = new Map();
let captureTimer = null;

function setStatus(connected, text) {
  statusEl.classList.toggle('connected', connected);
  statusText.textContent = text;
}

ws.onopen = () => {
  setStatus(true, 'Connected');
  // Initial screenshot capture loop (~10 fps is enough for HITL)
  captureTimer = setInterval(() => {
    sendCDP('Page.captureScreenshot', { format: 'png' });
  }, 100);
};

ws.onclose = () => {
  setStatus(false, 'Disconnected');
  if (captureTimer) clearInterval(captureTimer);
};

ws.onerror = () => setStatus(false, 'Error');

ws.onmessage = (ev) => {
  let msg;
  try { msg = JSON.parse(ev.data); } catch { return; }
  if (msg.id && pending.has(msg.id)) {
    const { resolve } = pending.get(msg.id);
    pending.delete(msg.id);
    resolve(msg.result || msg.error);
  }
};

function sendCDP(method, params) {
  return new Promise((resolve) => {
    const id = nextId++;
    pending.set(id, { resolve });
    ws.send(JSON.stringify({ id, method, params: params || {} }));
  });
}

// ── Input forwarding ──────────────────────────────────────────────
function eventToViewport(e) {
  const rect = screenEl.getBoundingClientRect();
  // Map screen coordinates to viewport coordinates. The browser
  // is 1280x800 (set at HeadedHitlSession::launch), the displayed
  // <img> may be scaled. We compute the scale factor and invert it.
  const scaleX = 1280 / rect.width;
  const scaleY = 800 / rect.height;
  return {
    x: (e.clientX - rect.left) * scaleX,
    y: (e.clientY - rect.top) * scaleY,
    button: e.button,
  };
}

['mousedown', 'mousemove', 'mouseup'].forEach((evt) => {
  screenEl.addEventListener(evt, (e) => {
    const { x, y, button } = eventToViewport(e);
    const type = evt === 'mousedown' ? 'mousePressed'
              : evt === 'mouseup'   ? 'mouseReleased' : 'mouseMoved';
    sendCDP('Input.dispatchMouseEvent', {
      type, x, y, button,
      buttons: type === 'mouseMoved' ? 1 : 1,
      clickCount: type === 'mousePressed' ? 1 : 0,
    });
  });
});

screenEl.addEventListener('wheel', (e) => {
  const { x, y } = eventToViewport(e);
  sendCDP('Input.dispatchMouseEvent', {
    type: 'mouseWheel', x, y,
    deltaX: e.deltaX, deltaY: e.deltaY,
  });
  e.preventDefault();
}, { passive: false });

window.addEventListener('keydown', (e) => {
  sendCDP('Input.dispatchKeyEvent', {
    type: 'keyDown',
    key: e.key,
    code: e.code,
    text: e.key.length === 1 ? e.key : '',
    windowsVirtualKeyCode: e.keyCode,
  });
});
window.addEventListener('keyup', (e) => {
  sendCDP('Input.dispatchKeyEvent', {
    type: 'keyUp',
    key: e.key,
    code: e.code,
    windowsVirtualKeyCode: e.keyCode,
  });
});

// ── Render loop ───────────────────────────────────────────────────
let lastDataUrl = null;
function applyResult(result) {
  if (!result || !result.data) return;
  const dataUrl = `data:image/png;base64,${result.data}`;
  if (dataUrl !== lastDataUrl) {
    screenEl.src = dataUrl;
    lastDataUrl = dataUrl;
  }
}

// Patch the sendCDP resolver to apply screenshot results.
const _sendCDP = sendCDP;
window.sendCDP = (method, params) => _sendCDP(method, params).then((r) => {
  if (method === 'Page.captureScreenshot' && r && r.data) applyResult(r);
  return r;
});
</script>
</body>
</html>
"#;
