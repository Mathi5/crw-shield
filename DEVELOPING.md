# Development

This document covers building, testing, and contributing to crw-shield.

## Requirements

- **Rust** ≥ 1.83 (stable). The workspace is tested on `1.88` (the version
  pinned in `rust-toolchain.toml`).
- **Docker** + **Docker Compose** for the CDP fetcher (Chromium) and for
  hermetic builds.
- **Go** ≥ 1.24 to build the `tls-impersonate-proxy` sidecar from source
  (not required if you use the prebuilt binary that ships in the Docker
  image).
- **CMake**, **build-essential**, **pkg-config**, **libclang-dev**, **perl** —
  required by [wreq](https://github.com/0x676e67/wreq)'s BoringSSL feature.

## Workspace layout

```
crw-shield/
├── crates/
│   ├── core/        # shared types: ScrapeRequest, ScrapeResponse, CrwError
│   ├── antibot/     # CookieJar, fingerprint detection, situation reports,
│   │                # browser profile rotation, TLS profile definitions
│   ├── extract/     # HTML → markdown conversion
│   ├── fetch/       # HTTP fetcher (reqwest), CDP fetcher (chromiumoxide),
│   │                # FlareSolverr client, TLS proxy sidecar, ladder
│   ├── crawl/       # Site crawler (BFS)
│   ├── search/      # Search engine abstraction
│   ├── map/         # Site mapping
│   └── server/      # Axum HTTP server, Firecrawl v2 routes, rate limiter,
│                    # state, handlers
├── tls-proxy/       # Go module for the tls-impersonate-proxy sidecar
├── benches/         # Out-of-tree benchmark scripts
├── bench/           # In-tree benchmark result docs
└── docs/            # Configuration, architecture notes
```

## Quick start

```bash
# 1. Clone
git clone https://github.com/Mathi5/crw-shield
cd crw-shield

# 2. Build (debug)
cargo build

# 3. Run tests
cargo test --all

# 4. Lint (CI runs this with -D warnings)
cargo clippy --all --all-targets -- -D warnings
cargo fmt --all -- --check

# 5. Run the server (debug binary)
RUST_LOG=debug cargo run --bin crw-server
```

The server listens on `0.0.0.0:3002` by default. Send a test request:

```bash
curl -X POST http://localhost:3002/v2/scrape \
  -H 'Content-Type: application/json' \
  -d '{"url": "https://example.com", "formats": ["markdown"]}'
```

## Docker workflow (recommended)

The repo ships a `Dockerfile` + `docker-compose.yml` that builds and runs
crw-shield in a single command. This is the workflow used by the bench
scripts and by the release process.

```bash
# Build the image (uses the cargo cache from your host for fast rebuilds)
docker compose build

# Start the server
docker compose up

# Tail logs
docker compose logs -f crw-shield

# Stop and remove
docker compose down
```

The image is `crw-shield-crw-shield:latest` (yes, the double name is a
Cargo workspace + Compose quirk — the binary inside is `crw-server`).

To rebuild from scratch (useful when the Cargo cache mis-detects a change
as no-op — see `docs/CARGO_CACHE_PITFALL.md`):

```bash
docker rmi -f crw-shield-crw-shield:latest
touch crates/fetch/src/ladder.rs  # or whichever .rs you changed
docker compose build
```

## Testing

```bash
# Unit tests (fast, no network)
cargo test --all --locked

# Integration tests
cargo test --test '*' -- --test-threads=1

# Doctests
cargo test --doc
```

A few test patterns are worth knowing:

- **`validate_flaresolverr_solution_*`** — pin the Light#4 / Light#5 logic
  for accepting FlareSolverr-resolved pages
- **`FlareSolverrAllowlist::is_allowed_*`** — pin exact + subdomain match
- **`RateLimiter::wait_*`** — pin the per-host delay logic

When you change a behavior that's covered by these tests, **read the test
first** — it usually documents the contract.

## Bench

The 30-site panel benchmark is in `benches/bench30.py` (sibling of the
`/tmp/bench30.py` script used during development). Run it against a running
crw-shield:

```bash
# Local
python3 benches/bench30.py http://localhost:3002

# With Docker
docker compose up -d
python3 benches/bench30.py http://localhost:3002
docker compose down
```

The script outputs a human-readable table and a CSV at
`/tmp/bench30_results.csv`. Add new sites to the `SITES` list — keep T1/T2/T3
balanced so the percentage stays meaningful.

## Debugging

- **`RUST_LOG=trace cargo run`** — full trace logs
- **`RUST_LOG=crw_fetch=trace`** — trace only the fetch crate
- **Browser DevTools** — the chromiumoxide fetcher can attach to a remote
  Chrome instance: set `CHROME_PATH=/path/to/chrome --remote-debugging-port=9222`
- **FlareSolverr logs** — `tail -f /var/log/flaresolverr.log` (or wherever
  your FS instance logs)

For pprof / heap profiling:

```bash
cargo install flamegraph
cargo flamegraph --bin crw-server
```

## Release process

See [`RELEASING.md`](RELEASING.md) for the full procedure (tagging, building
binaries for all targets, publishing to GitHub Releases).

## Contributing

1. **Branch** off `main`: `git checkout -b feat/short-name`
2. **Write tests first** if the change is non-trivial
3. **Keep commits focused** — one logical change per commit
4. **Update CHANGELOG.md** with a one-line description
5. **Run the full test + lint suite locally** before pushing
6. **Push your branch** and open a PR against `main`
7. **Wait for CI** — `cargo fmt`, `cargo clippy -D warnings`, `cargo test`
   must all pass

### Code style

- `cargo fmt` (the CI runs this — non-negotiable)
- `cargo clippy --all --all-targets -- -D warnings` (the CI also runs this)
- Prefer `tracing::{info, warn, debug, error}` over `println!` for any
  non-test code
- Use `?` for error propagation; define errors in `crates/core/src/error.rs`
  (or a sub-module) with `thiserror`

### Architecture invariants

- **The HTTP fetcher must never leak cookies across sites.** `CookieJar`
  scopes by host; the `cookie_header_for` helper walks parent domains but
  never siblings. If you add a new cookies API, keep this invariant.
- **FlareSolverr must never be called globally.** Always go through
  `FlareSolverrAllowlist::is_allowed` first. This is what kept cloudflare.com
  from regressing when FS was enabled globally (Pitfall 17).
- **L1 rotation clears cookies.** The `CookieJar::clear_for_host` method is
  called on each L1 rotation. Don't bypass it.

## License

By contributing, you agree that your contributions will be licensed under
the [MIT License](LICENSE).
