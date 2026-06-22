# tls-impersonate-proxy (crw-shield sidecar)

MITM HTTPS proxy that re-issues requests via [`bogdanfinn/tls-client`](https://github.com/bogdanfinn/tls-client) with an impersonated browser fingerprint (Chrome, Firefox, Safari). Used as a sidecar in crw-shield's Docker image to bypass Cloudflare IUAM and similar TLS-fingerprint-sensitive anti-bot checks.

## Architecture

```
Chrome --HTTPS--> 127.0.0.1:7890 (proxy)
                       │
                       ├─ CONNECT target:443 received
                       ├─ Generate per-host cert signed by <ca-dir>/ca.{crt,key}
                       ├─ Wrap conn in TLS server with the per-host cert
                       ├─ Decrypt Chrome's HTTP request
                       ├─ Re-issue via bogdanfinn/tls-client with --profile chrome_120
                       └─ Stream the response back to Chrome (re-encrypted in the tunnel)
```

## Origin

Originally from [CyrilLeblanc/cortex-bridge](https://forgejo.cyrleb.dev/CyrilLeblanc/cortex-bridge) (MIT, 2026). Integrated into crw-shield as a sidecar binary under the same MIT license.

## Build (standalone, for development)

```bash
cd tls-proxy
CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o tls-impersonate-proxy .
```

## Build (in crw-shield's Dockerfile)

The Dockerfile builds this binary in a separate stage and copies it into the runtime image. See `Dockerfile` stages `go-tls-proxy-builder` and the runtime `COPY --from=go-tls-proxy-builder`.

## Configuration (env vars, set by crw-shield's Rust parent)

| Env var | Default | Description |
|---|---|---|
| `TLS_PROXY_LISTEN` | `127.0.0.1:7890` | Listen address |
| `TLS_PROXY_PROFILE` | `chrome_120` | TLS profile name (`chrome_120`, `firefox_117`, `safari_16_0`, etc.) |
| `TLS_PROXY_CA_DIR` | `/var/lib/crw-shield/tls-ca` | Persistent CA dir (CA cert + key) |
| `TLS_PROXY_BYPASS` | `localhost,127.0.0.1,::1` | Hosts to forward as raw tunnel (no MITM) |
| `TLS_PROXY_TIMEOUT` | `60s` | Per-request timeout |

The Rust parent process (`crates/fetch/src/tls_proxy.rs`) spawns this binary as a child process, polls the listen port for readiness, and kills it on rotation (L2).

## Activation

The proxy is **opt-in** in crw-shield. Set `TLS_PROXY_ENABLED=true` in the environment to enable. When disabled, crw-shield runs exactly as before (no proxy, no behavior change, no regression risk).

## See also

- `crates/fetch/src/tls_proxy.rs` — Rust lifecycle (spawn/kill/readiness probe)
- `crates/fetch/src/cdp.rs` — Chromium args injected when proxy is enabled
- `bench/CORTEX_BRIDGE_BENCH.md` — empirical comparison vs crw-shield baseline
- `references/tls-impersonate-proxy.md` (in the `rust-anti-scraping-bypass` skill) — full architecture
