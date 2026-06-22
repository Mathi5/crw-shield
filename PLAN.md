# crw-shield — Plan d'implémentation

## Objectif

Un seul binaire Rust, API-compatible Firecrawl v2, avec anti-bot inspiré de CortexScout, léger (~50MB RAM), avec SearXNG en proxy externe (env var).

## Architecture

Cargo workspace avec ces crates :

```
crw-shield/
├── Cargo.toml                    # workspace
├── crates/
│   ├── core/                      # types communs, config, erreurs
│   ├── antibot/                   # anti-bot (porté de CortexScout)
│   ├── fetch/                     # HTTP fetch + browser render (CDP plus tard)
│   ├── extract/                  # HTML processing (markdown, links, metadata)
│   ├── crawl/                     # BFS crawl engine
│   ├── search/                   # proxy SearXNG
│   ├── map/                       # URL discovery
│   └── server/                   # axum API + CLI entry point
├── Dockerfile
├── docker-compose.yml
└── .github/workflows/ci.yml
```

## Crates Rust à utiliser

| Rôle | Crate | Version |
|------|-------|---------|
| HTTP server | axum | 0.8 |
| HTTP client | reqwest | 0.12 (features: json, gzip, brotli, deflate, cookies, socks) |
| Serialization | serde + serde_json | 1 |
| HTML parsing | scraper | 0.22 |
| HTML→Markdown | htmd | 0.1 (ou html2md 0.2) |
| Content extraction | readability | 0.3 (ou custom avec scraper) |
| Sitemap XML | quick-xml | 0.36 |
| Cache | moka | 0.12 (feature: future) |
| Job store | rusqlite | 0.32 (feature: bundled) |
| Config | env vars via std::env |  |
| Retry/backoff | backoff | 0.4 (feature: tokio) |
| Logging | tracing + tracing-subscriber | 0.1 / 0.3 |
| Error | thiserror | 2 |
| Utils | url, uuid, chrono, async-trait |  |
| CLI | clap | 4 (optionnel) |

## API Endpoints (compat Firecrawl v2)

| Endpoint | Méthode | Priorité | Description |
|----------|---------|----------|-------------|
| /v2/scrape | POST | P0 | Scrape single URL → markdown/html/links/screenshot |
| /v2/crawl | POST | P0 | Start async crawl, return job ID |
| /v2/crawl/{id} | GET | P0 | Poll crawl status + results |
| /v2/crawl/{id} | DELETE | P1 | Cancel crawl |
| /v2/map | POST | P0 | URL discovery (sitemap + links) |
| /v2/search | POST | P0 | Proxy vers SearXNG + optional scrape |
| /v2/batch/scrape | POST | P1 | Batch async scrape |
| /v2/batch/scrape/{id} | GET | P1 | Batch status |
| /v2/crawl/{id}/errors | GET | P2 | Crawl errors |

## Types Firecrawl v2 (à implémenter dans core/src/types.rs)

### ScrapeRequest (POST /v2/scrape)
```json
{
  "url": "https://example.com",          // required
  "formats": ["markdown", "html", "rawHtml", "links", "screenshot"],  // default: ["markdown"]
  "onlyMainContent": true,                // default true
  "includeTags": ["h1", "article"],       // optional
  "excludeTags": ["nav", "footer"],       // optional
  "headers": {},                          // custom HTTP headers
  "waitFor": 0,                           // ms delay before fetch
  "mobile": false,
  "skipTlsVerification": false,
  "timeout": 60000,                       // ms, default 60000
  "actions": [                            // optional, browser actions
    {"type": "wait", "milliseconds": 2000},
    {"type": "click", "selector": "#btn"},
    {"type": "screenshot", "fullPage": true}
  ],
  "removeBase64Images": false,
  "blockAds": true,
  "proxy": "auto",                        // "basic" | "enhanced" | "auto"
  "maxAge": 172800000,                    // cache max age in ms, default 48h
  "storeInCache": true
}
```

### ScrapeResponse
```json
{
  "success": true,
  "data": {
    "markdown": "...",
    "html": "...",          // optional
    "rawHtml": "...",        // optional
    "links": ["https://..."], // optional
    "screenshot": "data:image/png;base64,...", // optional
    "metadata": {
      "title": "...",
      "description": "...",
      "language": "en",
      "sourceURL": "https://...",
      "url": "https://...",
      "statusCode": 200,
      "error": null
    }
  }
}
```

### CrawlRequest (POST /v2/crawl)
```json
{
  "url": "https://example.com",
  "excludePaths": ["blog/.*"],
  "includePaths": ["docs/.*"],
  "maxDiscoveryDepth": null,
  "sitemap": "include",          // "skip" | "include" | "only"
  "ignoreQueryParameters": true,
  "regexOnFullURL": false,
  "limit": 10000,
  "crawlEntireDomain": false,
  "allowExternalLinks": false,
  "allowSubdomains": false,
  "ignoreRobotsTxt": false,
  "delay": null,                 // seconds between scrapes
  "maxConcurrency": null,
  "scrapeOptions": { /* same as ScrapeRequest minus url */ }
}
```

### CrawlResponse (async)
```json
{
  "success": true,
  "id": "uuid-here",
  "url": "https://api.../v2/crawl/uuid-here",
  "creditsUsed": 1
}
```

### CrawlStatusResponse (GET /v2/crawl/{id})
```json
{
  "status": "scraping",    // "scraping" | "completed" | "failed" | "cancelled"
  "total": 50,
  "completed": 50,
  "creditsUsed": 50,
  "expiresAt": "2026-...",
  "createdAt": "2026-...",
  "completedAt": "2026-...",
  "duration": 120.5,
  "next": null,            // pagination URL
  "data": [/* ScrapeData[] */]
}
```

### MapRequest (POST /v2/map)
```json
{
  "url": "https://example.com",
  "search": "blog",         // optional filter
  "sitemap": "include",
  "includeSubdomains": false,
  "ignoreQueryParameters": true,
  "ignoreCache": false,
  "limit": 5000,
  "timeout": null
}
```

### MapResponse
```json
{
  "success": true,
  "links": [
    {"url": "https://...", "title": "...", "description": "..."}
  ]
}
```

### SearchRequest (POST /v2/search)
```json
{
  "query": "rust web scraper",
  "limit": 10,              // 1-100
  "sources": ["web"],       // "web" | "images" | "news"
  "includeDomains": ["example.com"],
  "excludeDomains": ["bad.com"],
  "tbs": "qdr:w",
  "timeout": 60000,
  "ignoreInvalidURLs": false,
  "scrapeOptions": { /* optional scrape options for results */ }
}
```

### SearchResponse
```json
{
  "success": true,
  "data": {
    "web": [{"title": "...", "description": "...", "url": "...", "markdown": "..."}],
    "images": [],
    "news": []
  },
  "warning": null,
  "id": "...",
  "creditsUsed": 10
}
```

### ErrorResponse
```json
{
  "success": false,
  "error": "INVALID_URL",
  "message": "URL is not valid"
}
```

## Anti-bot (crates/antibot/) — Porté de CortexScout

### Layer 1: HTTP Stealth (http_stealth.rs)
- 17 user-agents rotatés (Chrome/Firefox/Safari/Edge, desktop + mobile)
- 5 browser profiles complets avec sec-ch-ua headers + viewport dimensions
- Headers complets: Accept, Accept-Language, Accept-Encoding, DNT, Connection, Upgrade-Insecure-Requests, Sec-Fetch-Dest/Mode/Site, Cache-Control
- Rate limiting avec jitter ±20%, 3 presets: polite (500-1500ms), aggressive (100-500ms), conservative (1000-3000ms)
- Atomic rate limiter thread-safe

### Layer 2: CDP Stealth (cdp_stealth.rs) — Phase 2, pas dans le PoC
- Script JS injecté via AddScriptToEvaluateOnNewDocument
- navigator.webdriver → undefined
- Canvas fingerprint noise
- WebGL spoofing (Intel Iris)
- chrome.runtime stub
- Permissions API bypass
- Client hints spoofing
- Suppression marqueurs automation (Playwright, Puppeteer, Selenium, Phantom)

### Challenge Detection (challenge_detect.rs)
```rust
fn detect_challenge(html: &str) -> Option<String> {
    if html.contains("challenges.cloudflare.com") { return Some("Cloudflare".into()); }
    if html.contains("hcaptcha.com") { return Some("hCaptcha".into()); }
    if html.contains("recaptcha") { return Some("reCAPTCHA".into()); }
    if html.contains("perimeterx") { return Some("PerimeterX".into()); }
    if html.contains("datadome.co") { return Some("DataDome".into()); }
    None
}
```

## Variables d'environnement

```bash
PORT=3002
HOST=0.0.0.0
AUTH_TOKEN=           # optionnel
SEARXNG_URL=http://localhost:8080
SEARXNG_TOKEN=         # optionnel
CACHE_TTL_SECS=172800
CRAWL_MAX_CONCURRENCY=5
CRAWL_DEFAULT_LIMIT=10000
SCRAPE_DELAY_PRESET=polite
STEALTH_ENABLED=true
BEHAVIORAL_SIMULATION=true
PROXY_URL=
PROXY_FILE=
PROXY_STICKY_SESSIONS=true
PROXY_COOLDOWN_SECS=300
```

## Pipeline de scrape (PoC — HTTP only)

```
POST /v2/scrape {url, formats, options}
  │
  ├─→ Cache check (moka): résultat < maxAge?
  │   YES → return cached
  │   NO  → continue
  │
  ├─→ HTTP fetch (reqwest + stealth headers + UA rotation)
  │   ├─ Challenge détecté? → return error (CDP en Phase 2)
  │   └─ HTML reçu → continue
  │
  ├─→ Extract:
  │   ├─ onlyMainContent (readability ou custom)
  │   ├─ includeTags/excludeTags
  │   ├─ HTML → Markdown
  │   ├─ Extract metadata (title, description, OG, lang)
  │   ├─ Extract links if requested
  │   └─ Screenshot si demandé (erreur si pas de CDP en Phase 1)
  │
  ├─→ Cache (moka, TTL maxAge) → return {success, data: {...}}
```

## Tests

CRITIQUE: Chaque crate doit avoir des tests unitaires. Le server doit avoir des tests d'intégration.

### Tests unitaires par crate:
- **core**: désérialisation des requests, sérialisation des responses, config from env
- **antibot**: UA rotation (pas de répétition sur N tirages), challenge detection (cas positif/négatif), delay preset ranges
- **extract**: HTML→markdown (HTML simple), extraction links (HTML avec liens), extraction metadata (HTML avec OG tags), onlyMainContent (strip nav/footer)
- **fetch**: (mock HTTP — utiliser wiremock ou httpmock)
- **crawl**: URL filtering (domain match, path regex, robots.txt)
- **search**: (mock SearXNG response)
- **map**: sitemap XML parsing, link extraction from HTML

### Tests d'intégration (server/tests/):
- POST /v2/scrape avec URL réelle ou mock
- POST /v2/scrape avec URL invalide → error response
- POST /v2/crawl → GET /v2/crawl/{id} → status
- POST /v2/map avec URL de test
- POST /v2/search (skip si pas de SEARXNG_URL)
- Auth middleware (si AUTH_TOKEN set)

## CI/CD GitHub Actions

```yaml
name: CI
on: [push, pull_request]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --check
      - run: cargo clippy -- -D warnings
      - run: cargo test --all
```

## PoC Scope (Phase 1) — DONE

## Phase 2 Scope — CDP Rendering + Crawl + Map + Search

### Objectifs
1. **CDP rendering** avec chromiumoxide — scrape de pages JS-rendered
2. **Screenshots** via CDP `Page.captureScreenshot`
3. **Crawl endpoint** — POST /v2/crawl + GET /v2/crawl/{id} + DELETE /v2/crawl/{id}
4. **Map endpoint** — POST /v2/map (URL discovery via sitemap + links)
5. **Search endpoint** — POST /v2/search (proxy vers SearXNG)
6. **Stealth CDP** — injection du script JS anti-detection avant le render

### Détails par crate

#### crates/antibot/src/cdp_stealth.rs (NOUVEAU)
Script JS injecté via `Page.addScriptToEvaluateOnNewDocument`:
- `navigator.webdriver` → `undefined`
- `navigator.languages` → `['en-US', 'en']`
- `navigator.plugins` → array non-vide
- `window.chrome.runtime` avec stubs `connect()` et `sendMessage()`
- `chrome.csi()` et `chrome.loadTimes()` avec timestamps plausibles
- Permissions API bypass: intercept `navigator.permissions.query()`
- Canvas fingerprint noise: wrap `getContext()` → `toDataURL()`, inject noise
- WebGL spoofing: `WebGLRenderingContext.getParameter()` retourne "Intel Inc." (37445) et "Intel Iris OpenGL Engine" (37446)
- Suppression marqueurs: `window.__playwright`, `window.__puppeteer`, `window.__selenium`, `window.callPhantom`, `window._phantom`
- Client hints: `navigator.userAgentData` avec brands array réaliste

Export: `pub fn stealth_script() -> &'static str` — retourne le script JS complet

#### crates/fetch/src/cdp.rs (NOUVEAU)
- `CdpFetcher` struct qui utilise chromiumoxide
- Détection automatique du browser (CHROME_PATH env var ou auto-detect)
- Launch options: `--disable-blink-features=AutomationControlled`, `--headless=new`, `--no-sandbox`, `--disable-gpu`, `--user-data-dir` temporaire
- Injecte le stealth script avant navigation
- Navigate → attend network idle ou timeout
- Execute actions (click, wait, screenshot, press, scroll, write)
- Capture screenshot si demandé (format base64 data URI)
- Retourne FetchResult { html, status_code, final_url, screenshot }
- Browser pool: garde le browser ouvert, un tab par requête

#### crates/fetch/src/ladder.rs (NOUVEAU)
- `FetchLadder` qui orchestre le fallback progressif:
  1. HTTP fetch simple (fast path) — si HTML statique suffisant, retourne
  2. Si challenge détecté OU actions demandées OU HTML vide → CDP rendering
  3. Si CDP aussi échoue → erreur

#### crates/fetch/src/lib.rs (MODIFIER)
- Ajouter `CdpFetcher` et `FetchLadder` aux exports
- Le `Fetcher` trait reste le même, CdpFetcher l'implémente

#### crates/crawl/src/lib.rs (IMPLÉMENTER COMPLÈTEMENT)
- `CrawlEngine` struct
- `start_crawl(url, options) -> String` (retourne job ID UUID)
- `get_crawl_status(id) -> CrawlStatusResponse`
- `cancel_crawl(id) -> CancelResponse`
- Job store: SQLite via rusqlite (table: crawl_jobs avec id, url, status, total, completed, data JSON, created_at, completed_at)
- BFS crawl: 
  1. Phase découverte: fetch sitemap.xml + extraire liens de la page d'origine
  2. Filtrer les URLs (domaine, paths, robots.txt, query params)
  3. Scraper chaque URL avec FetchLadder (HTTP puis CDP si besoin)
  4. Extraire nouveaux liens de chaque page
  5. Continuer jusqu'à: pas de nouveaux liens | limit atteint | max depth
- Concurrency: tokio tasks avec semaphore (max_concurrency)
- Pagination des résultats (10MB max par response, `next` URL si plus)

#### crates/map/src/lib.rs (IMPLÉMENTER COMPLÈTEMENT)
- `discover_urls(url, options) -> MapResponse`
- Fetch sitemap.xml (suivre sitemap index → child sitemaps)
- Si sitemap mode = "include": aussi fetch la page HTML et extraire <a> tags
- Filtrer: includeSubdomains, ignoreQueryParameters
- Si search fourni: filtrer par pertinence (matching simple sur titre/description)
- Cache: moka avec TTL 7 jours pour les sitemaps

#### crates/search/src/lib.rs (IMPLÉMENTER COMPLÈTEMENT)
- `SearxngClient` struct
- `search(query, options) -> SearchResponse`
- POST vers `{SEARXNG_URL}/search` avec `format=json`
- Headers: Authorization si SEARXNG_TOKEN set
- Mapper la réponse SearXNG vers SearchResponse Firecrawl
- Si scrapeOptions fourni: scraper chaque résultat avec FetchLadder

#### crates/server/src/routes.rs (MODIFIER)
- Ajouter routes: POST /v2/crawl, GET /v2/crawl/{id}, DELETE /v2/crawl/{id}
- Ajouter route: POST /v2/map
- Ajouter route: POST /v2/search
- Le handler /v2/scrape utilise maintenant FetchLadder (HTTP puis CDP fallback)

#### crates/server/src/handlers.rs (MODIFIER)
- `scrape_handler`: utilise FetchLadder au lieu de HttpFetcher direct
- `crawl_handler`: démarre CrawlEngine en background
- `crawl_status_handler`: lit SQLite
- `crawl_cancel_handler`: cancel le job
- `map_handler`: utilise map::discover_urls
- `search_handler`: utilise search::SearxngClient

#### Cargo.toml (MODIFIER)
Ajouter aux workspace dependencies:
- `chromiumoxide = { version = "0.7", features = ["tokio-runtime"] }` (ou 0.6 si 0.7 n'existe pas)
- `rusqlite = { version = "0.32", features = ["bundled"] }`
- `moka = { version = "0.12", features = ["future"] }`
- `futures = "0.3"`
- `quick-xml = "0.36"`
- `regex = "1"`

#### crates/fetch/Cargo.toml (MODIFIER)
- Ajouter: chromiumoxide, futures, crw-antibot (déjà présent)

#### crates/crawl/Cargo.toml (MODIFIER)
- Ajouter: rusqlite, moka, regex, quick-xml, tokio (déjà présent)

#### crates/map/Cargo.toml (MODIFIER)
- Ajouter: moka, quick-xml, regex

#### crates/search/Cargo.toml (MODIFIER)
- Ajouter: moka (optionnel pour cache)

### Tests requis pour Phase 2

#### antibot
- `cdp_stealth.rs`: test que `stealth_script()` contient les éléments clés (webdriver, chrome.runtime, WebGL, canvas)
- Test que le script est valide JS (pas de syntax error évidente)

#### fetch
- `cdp.rs`: test que CdpFetcher peut lancer un browser et scraper une page simple (test d'intégration avec chromium — peut être ignoré si CHROME_PATH non set avec `#[ignore]`)
- `ladder.rs`: test que le ladder utilise HTTP first, CDP seulement si besoin (mock)

#### crawl
- Test URL filtering: domain match, subdomain, path regex, excludePaths, includePaths
- Test sitemap parsing: XML valide → URLs
- Test robots.txt parsing (si implémenté)
- Test crawl engine: mock fetcher + petit site → vérifier BFS découvre les bonnes URLs

#### map
- Test sitemap XML parsing (sitemap simple + sitemap index)
- Test link extraction from HTML
- Test filtering (subdomains, query params)

#### search
- Test avec mock SearXNG response (utiliser un serveur mock)
- Test mapping SearXNG → Firecrawl format
- Test avec includeDomains/excludeDomains filtering

#### server (tests d'intégration)
- POST /v2/crawl → GET /v2/crawl/{id} → status completed
- POST /v2/map avec mock
- POST /v2/search avec mock SearXNG (ou skip si pas de SEARXNG_URL)
- POST /v2/scrape avec screenshot format → retourne screenshot base64 (test d'intégration avec chromium, `#[ignore]` si pas de browser)

### Important
- chromiumoxide nécessite Chrome/Chromium installé. Dans Docker, c'est géré par le Dockerfile.dev.
- Les tests qui nécessitent un browser réel doivent utiliser `#[ignore]` avec un commentaire `// Run with: cargo test -- --ignored`
- Le crawl job store utilise SQLite (fichier temporaire ou :memory: pour les tests)
- SearXNG n'est pas dans le container — les tests search utilisent un mock
- TOUT doit compiler et passer avec `cargo test --all` (les tests `#[ignore]` ne sont pas exécutés par défaut)
- `cargo clippy -- -D warnings` doit être clean
- `cargo fmt -- --check` doit être clean