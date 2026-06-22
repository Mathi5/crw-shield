# Changelog

All notable changes to crw-shield are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

### Bench
- 30-site panel: 29/30 = 96.7% / 299 700 chars (residential Freebox IP)
- T1: 7/7 = 100%, T2: 11/12 = 92%, T3: 11/11 = 100%
- Single remaining failure: youtube.com (streaming, out of scope)

[Unreleased]: https://github.com/Mathi5/crw-shield/compare/main...HEAD
