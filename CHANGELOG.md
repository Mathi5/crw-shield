# Changelog

All notable changes to crw-shield are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Optional `firecrawl-extractor` feature in `crw-extract` that uses
  [firecrawl/html-extractor](https://github.com/firecrawl/html-extractor)
  (Apache-2.0) for `Article` and `Doc` page types. Provides a 5-stage
  extraction pipeline (pre-clean → classify → score → fallback → render)
  with page-type-aware scoring weights. See `crates/extract/NOTICE` for
  full attribution.
- `extract_main_content_v4`: page-type-aware router that delegates
  `Article` / `Doc` to the Firecrawl pipeline and keeps
  `Product` / `Listing` / `Forum` / `Service` / `Collection` / `Unknown`
  on the existing v3 path. With the feature off, v4 is behaviorally
  identical to v3 (no overhead, no API change).
- `schema_org_data: Option<serde_json::Value>` field in `ScrapeMetadata`.
  Captures the first `<script type="application/ld+json">` block when
  present, so clients can do typed entity extraction (Recipe, Product,
  Article, etc.) without re-fetching.
- `PageType::Collection` and `PageType::Service` variants added to
  match the Firecrawl taxonomy (was 6 variants, now 8).
- `should_retry_for_quality` in `crw-fetch` documented as ready for
  follow-up wiring (already implemented + tested; live retry loop
  deferred to a separate change).

### Notes
- Default build is **unchanged**: no new deps compiled unless
  `--features crw-extract/firecrawl-extractor` is passed. Build time
  delta with the feature on: ~30s cold, ~3s warm.
- Bench (inline fixtures, release build): v4 adds ~0.15ms over v3
  per `Article` / `Doc` page (v3 pre-pass + Firecrawl re-parse).
  `Product` / `Forum` are bit-identical to v3, confirming the router.

### Fixed
- **Phase D.1 (situation-aware routing)**: `extract_main_content_v4`
  now consults the upstream `SituationReport` before delegating to
  Firecrawl. When `situation.kind.is_anti_bot()` is true (Cloudflare
  IUAM, DataDome, Kasada, Akamai, PerimeterX, etc.), Firecrawl is
  bypassed and v3's situation-aware result is returned. This prevents
  Firecrawl from extracting the challenge page itself as if it were
  content (which happened with perimeterx-demo in the v0.1.0 bench).

### Bench (30-site panel, real network, post-D.1)
- **Headline**: wikipedia +370% bytes and +5× quality score vs
  v3; twitter +3.5× quality; cloudflare.com +10× quality at 50%
  the bytes (Firecrawl noise filtering); bbc-news and lemonde
  bit-identical (v3 already optimal).
- **Anti-bot correctly bypassed**: perimeterx-demo no longer returns
  153 bytes of challenge-page garbage extracted by Firecrawl — it
  now escalates to `HITL_REQUIRED` like every other anti-bot site.
- **Aggregate** (23 sites OK in both v0.1.0 baseline and v4d):
  +32.5% bytes, **+33% mean quality score** (0.198 → 0.263).
- Caveats: bench binaire ran on host with `CHROME_PATH=/snap/bin/chromium`
  and local FS/CDP ladder (no FlareSolverr/TLS proxy like the docker
  build), so a few Tier 3 sites appear as `HITL_REQUIRED` on the host
  where the docker v0.1.0 baseline succeeded. This is an environment
  difference, not a Phase D.1 regression.

## [0.1.0] - 2026-06-22

First public release.

### Added
- Sous-phase 2: cookie injection post-FS into shared CookieJar with
  domain-match filtering (`commit 8bb19d2`)
- Light#5: per-fingerprint DataDome threshold (5 000 → 1 000 chars)
  for FlareSolverr-resolved pages (`commit 8bb19d2`)
- Sous-phase 1: `FlareSolverrAllowlist` with exact + subdomain match,
  opt-in per host via `FLARESOLVERR_HOSTS` env var (`commit f6640bf`)
- Light#4: large-resolved-page escape hatch in
  `validate_flaresolverr_solution` (`commit f6640bf`)
- Phase 3: per-host rate limiter (opt-in via `RATE_LIMIT_MIN_MS`)
  (`commit bda134c`)
- Phase 2: profile warming (opt-in via `CRW_WARMUP_ENABLED=true`,
  currently disabled by default due to a chromiumoxide Browser lock bug)
  (`commit 13c6bc2`)
- Phase 1: TLS proxy (`bogdanfinn/tls-impersonate-proxy`) enabled by
  default via `TLS_PROXY_ENABLED=true` (`commit c7bed40`)
- First-class documentation: README, DEVELOPING, RELEASING, CHANGELOG,
  docs/CONFIGURATION, docs/CARGO_CACHE_PITFALL, MIT LICENSE
  (`commit 35bb168`)
- Automated release pipeline (`.github/workflows/release.yml`):
  multi-platform build (linux x86_64/arm64, darwin x86_64/arm64),
  GitHub Release with attached binaries, Docker image push to GHCR

### Bench (30-site panel, residential IP)
- 29/30 = 96.7% / 299 700 chars
- T1: 7/7 = 100%, T2: 11/12 = 92%, T3: 11/11 = 100%
- Single remaining failure: youtube.com (streaming, out of scope)
- Exceeds cortex-bridge reference (28/30 = 93% / 626 041 chars) in
  success rate

[0.1.0]: https://github.com/Mathi5/crw-shield/releases/tag/v0.1.0
[Unreleased]: https://github.com/Mathi5/crw-shield/compare/v0.1.0...HEAD
