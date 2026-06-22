# Configuration reference

All configuration is via environment variables. Defaults shown in `[]`.

## Server

| Variable | Default | Description |
|----------|---------|-------------|
| `HOST` | `0.0.0.0` | Bind address |
| `PORT` | `3002` | Bind port |
| `RUST_LOG` | `info` | `tracing` log level (`trace`, `debug`, `info`, `warn`, `error`) |
| `CRW_MAX_CONCURRENCY` | `32` | Max concurrent scrape requests |

## TLS proxy (L2 rotation)

| Variable | Default | Description |
|----------|---------|-------------|
| `TLS_PROXY_ENABLED` | `true` | Spawn the `tls-impersonate-proxy` sidecar at startup |
| `TLS_PROXY_BINARY` | `/usr/local/bin/tls-impersonate-proxy` | Path to the sidecar binary |
| `TLS_PROXY_LISTEN` | `127.0.0.1:7890` | Sidecar listen address |
| `TLS_PROXY_PROFILE` | `chrome_120` | Default TLS profile to spawn with |
| `TLS_PROXY_CA_DIR` | `/var/lib/crw-shield/tls-ca` | Persistent CA cache dir |
| `ROTATION_DELAY_SECS` | `3` | L2 cooldown between profile rotations |

## Browser profile rotation

| Variable | Default | Description |
|----------|---------|-------------|
| `USER_AGENT_ROTATION` | `true` | Rotate UA across 5 Chrome/Firefox/Safari profiles |
| `ROTATION_DELAY_SECS` | `3` | Cooldown (shared with TLS proxy) |

## CDP fetcher

| Variable | Default | Description |
|----------|---------|-------------|
| `CHROME_PATH` | `/usr/bin/chromium` | Chromium / Chrome binary path |
| `CHROME_HEADLESS` | `true` | Run headless |
| `CRW_WARMUP_ENABLED` | `false` | Profile warming (opt-in; see warning below) |

> **Warning** — `CRW_WARMUP_ENABLED=true` is currently **disabled by default**
> because [chromiumoxide](https://github.com/mattsse/chromiumoxide) leaks a
> `SingletonLock` when the warmup browser shares a profile with the main
> fetcher. Tracked upstream; see `bench/A_B_30SITE_BENCH.md` for the full
> diagnosis. Enabling warming in production requires a patched build or
> an ephemeral-browser workaround.

## FlareSolverr

| Variable | Default | Description |
|----------|---------|-------------|
| `FLARESOLVERR_URL` | (unset) | FlareSolverr v2 endpoint, e.g. `http://flaresolverr:8191` |
| `FLARESOLVERR_HOSTS` | (unset) | Comma-separated allowlist, e.g. `nowsecure.nl,perimeterx.com,etsy.com` |
| `FLARESOLVERR_TIMEOUT_MS` | `60000` | Client-side request timeout to FlareSolverr |

Wildcards are supported: `*.example.com` matches any subdomain.

## Rate limiting

| Variable | Default | Description |
|----------|---------|-------------|
| `RATE_LIMIT_MIN_MS` | `0` | Per-host minimum delay in milliseconds (0 = disabled) |
| `RATE_LIMIT_JITTER_MS` | `500` | Random jitter added to the delay |

## Profile (cookie persistence)

| Variable | Default | Description |
|----------|---------|-------------|
| `CRW_PROFILE_DIR` | `/var/lib/crw-shield/profile` | Persistent Chrome user-data-dir for cookie / session reuse across restarts |

## Example: full compose-style env file

```bash
# .env (do not commit)
HOST=0.0.0.0
PORT=3002
RUST_LOG=info
TLS_PROXY_ENABLED=true
TLS_PROXY_PROFILE=chrome_120
ROTATION_DELAY_SECS=3
CHROME_PATH=/usr/bin/chromium
FLARESOLVERR_URL=http://flaresolverr:8191
FLARESOLVERR_HOSTS=nowsecure.nl,perimeterx.com,kasada.io,datadome.co,leboncoin.fr,etsy.com
FLARESOLVERR_TIMEOUT_MS=60000
RATE_LIMIT_MIN_MS=0
RATE_LIMIT_JITTER_MS=500
CRW_WARMUP_ENABLED=false
```
