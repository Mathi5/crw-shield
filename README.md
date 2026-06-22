# crw-shield

**A Rust anti-bot scraper with a Firecrawl v2-compatible API.**

crw-shield is a single-binary HTTP server that fetches web pages and returns
clean markdown / HTML, even when the target site is protected by Cloudflare
IUAM, DataDome, Kasada, PerimeterX, Akamai, or other modern anti-bot systems.

It speaks the [Firecrawl v2 API](https://docs.firecrawl.dev/api-reference/endpoint/scrape)
so it can be used as a drop-in replacement for [Firecrawl](https://firecrawl.dev).

## Bench (30-site panel, residential IP)

| Tier | Anti-bot | crw-shield |
|-----:|----------|-----------:|
| T1   | none     | 7/7 = 100% |
| T2   | medium   | 11/12 = 92% |
| T3   | strict   | 11/11 = 100% |
| **Total** | | **29/30 = 96.7%** |

Single remaining failure: youtube.com (streaming, out of scope).

See [`bench/SOUS_PHASE_2_BENCH.md`](bench/SOUS_PHASE_2_BENCH.md) for the full
table and `bench/A_B_30SITE_BENCH.md` for the A/B comparison against
[cortex-bridge](https://forgejo.cyrleb.dev/CyrilLeblanc/cortex-bridge).

## Features

- **Firecrawl v2-compatible API** — `POST /v2/scrape` with formats `markdown`,
  `html`, `summary`, etc.
- **TLS fingerprinting** via [wreq](https://github.com/0x676e67/wreq) +
  optional [bogdanfinn/tls-impersonate-proxy](https://github.com/bogdanfinn/tls-impersonate-proxy)
  sidecar — byte-perfect Chrome / Firefox / Safari ClientHello
- **CDP fetcher** via [chromiumoxide](https://github.com/mattsse/chromiumoxide) —
  real headless Chromium for JS-heavy sites
- **FlareSolverr escalation** — opt-in per host allowlist, with cookie
  injection back into the shared CookieJar
- **Adaptive ladder** — HTTP → CDP → FlareSolverr, with rotation between
  5 browser profiles
- **HITL (Human-in-the-Loop) queue** — if the ladder exhausts, the request is
  enqueued for manual resolution and the cookies can be retrieved via
  `GET /v2/scrape/hitl/result?id=<uuid>`
- **Residential-IP friendly** — works with cheap or free residential proxies
  because the browser profile, TLS fingerprint, and HTTP/2 SETTINGS are
  consistent with a real browser

## Installation (binary)

Download the latest release for your platform from the
[Releases page](https://github.com/Mathi5/crw-shield/releases).

Pre-built binaries are published automatically on every Git tag push (see
[`RELEASING.md`](RELEASING.md) for the build matrix).

### Linux (x86_64)

```bash
curl -L -o crw-shield.tar.gz \
  https://github.com/Mathi5/crw-shield/releases/latest/download/crw-shield-linux-x86_64.tar.gz
tar -xzf crw-shield.tar.gz
sudo mv crw-shield /usr/local/bin/
```

### macOS (Apple Silicon)

```bash
curl -L -o crw-shield.tar.gz \
  https://github.com/Mathi5/crw-shield/releases/latest/download/crw-shield-darwin-aarch64.tar.gz
tar -xzf crw-shield.tar.gz
sudo mv crw-shield /usr/local/bin/
```

### Docker (recommended for the CDP fetcher)

The binary alone is enough for the HTTP + FlareSolverr paths. For the CDP
path (real headless Chromium), use the published image — it bundles
Chromium and all native dependencies.

```bash
docker pull ghcr.io/Mathi5/crw-shield:latest
docker run --rm -p 3002:3002 ghcr.io/Mathi5/crw-shield:latest
```

## Quick start

```bash
# Start the server (defaults: 0.0.0.0:3002, with TLS proxy on localhost:7890)
crw-shield

# In another terminal, scrape a page
curl -X POST http://localhost:3002/v2/scrape \
  -H 'Content-Type: application/json' \
  -d '{
    "url": "https://www.rust-lang.org",
    "formats": ["markdown"]
  }'
```

The response is a Firecrawl v2 `ScrapeResponse`:

```json
{
  "success": true,
  "data": {
    "markdown": "# Rust Programming Language\n\n...",
    "metadata": {
      "title": "Rust Programming Language",
      "scrapeEngine": "http"
    }
  }
}
```

## Configuration

All configuration is via environment variables. See
[`docs/CONFIGURATION.md`](docs/CONFIGURATION.md) for the full list. The most
useful ones:

| Variable | Default | Description |
|----------|---------|-------------|
| `HOST` | `0.0.0.0` | Bind address |
| `PORT` | `3002` | Bind port |
| `RUST_LOG` | `info` | tracing log level (`debug`, `info`, `warn`, `error`) |
| `TLS_PROXY_ENABLED` | `true` | Spawn the bogdanfinn/tls-impersonate-proxy sidecar |
| `TLS_PROXY_PROFILE` | `chrome_120` | Default TLS profile |
| `ROTATION_DELAY_SECS` | `3` | L2 cooldown between profile rotations |
| `CHROME_PATH` | `/usr/bin/chromium` | Path to the Chromium binary for CDP |
| `FLARESOLVERR_URL` | (unset) | FlareSolverr v2 endpoint, e.g. `http://localhost:8191` |
| `FLARESOLVERR_HOSTS` | (unset) | Comma-separated allowlist of hosts to escalate |
| `RATE_LIMIT_MIN_MS` | `0` | Per-host minimum delay (opt-in, 0 = off) |
| `CRW_WARMUP_ENABLED` | `false` | Profile warming (opt-in, see warning below) |

> **Warning** — `CRW_WARMUP_ENABLED=true` is currently **disabled by default**
> because [chromiumoxide](https://github.com/mattsse/chromiumoxide) leaks a
> `SingletonLock` when the warmup browser shares a profile with the main
> fetcher. This is tracked upstream; see `bench/A_B_30SITE_BENCH.md` for the
> full diagnosis. Enabling warming in production requires a patched build
> or an ephemeral-browser workaround.

## License

[MIT](LICENSE) — Copyright (c) 2026 crw-shield contributors.

Portions derived from [cortex-bridge](https://forgejo.cyrleb.dev/CyrilLeblanc/cortex-bridge),
also MIT licensed (Copyright (c) Cyril Leblanc and cortex-bridge contributors).

## Development

See [`DEVELOPING.md`](DEVELOPING.md) for the dev workflow (build from source,
run tests, debug, contribution guide).

## Acknowledgements

- [cortex-bridge](https://forgejo.cyrleb.dev/CyrilLeblanc/cortex-bridge) —
  primary source of inspiration and the `tls-impersonate-proxy` + rotation
  + HITL pattern. MIT licensed, ported and adapted.
- [Firecrawl](https://firecrawl.dev) — API compatibility target.
- [bogdanfinn/tls-impersonate-proxy](https://github.com/bogdanfinn/tls-impersonate-proxy) — TLS
  fingerprinting sidecar.
- [wreq](https://github.com/0x676e67/wreq) — TLS fingerprinting in-process.
- [chromiumoxide](https://github.com/mattsse/chromiumoxide) — CDP client.
- [FlareSolverr](https://github.com/FlareSolverr/FlareSolverr) — CF IUAM bypass.
