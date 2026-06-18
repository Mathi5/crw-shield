# Cloudflare Bypass Research — Non-HITL Solutions for crw-shield

## Context

crw-shield currently uses CDP (Chrome DevTools Protocol) with Chromium for rendering
JavaScript, but Cloudflare's "Just a moment..." JS challenge is not bypassed on sites
like StackOverflow. This document evaluates non-HITL (no human-in-the-loop) approaches
to solve Cloudflare challenges automatically.

---

## Approaches Evaluated

### 1. FlareSolverr (External Proxy)

**What:** A standalone proxy server (Docker container) that uses undetected-chromedriver
+ Selenium to solve Cloudflare challenges. It exposes a simple HTTP API on port 8191.

**API:**
```bash
curl -L -X POST 'http://localhost:8191/v1' \
  -H 'Content-Type: application/json' \
  --data-raw '{"cmd":"request.get","url":"https://stackoverflow.com","maxTimeout":60000}'
```

Response includes `solution.url`, `solution.status`, `solution.headers`, `solution.response` (HTML),
and `solution.cookies` (can be reused with any HTTP client).

Session management via `sessions.create` / `sessions.destroy` keeps a browser alive
and reuses cookies across requests — avoids solving the challenge every time.

**Pros:**
- Battle-tested, 14.4k GitHub stars, active maintenance (v3.5.0 May 2026)
- Drop-in Docker container — no Rust code changes needed for the solver itself
- Session management for cookie reuse (persistent browser instances)
- Works with Cloudflare and DDoS-GUARD
- MIT licensed

**Cons:**
- Each request launches (or reuses) a browser — high memory usage
- Adds a dependency: another Docker container in the stack
- Latency: challenge solving takes 5-15 seconds per new session
- Python/Selenium underneath — not Rust-native
- Some sites with advanced Cloudflare (Turnstile + behavioral) may still fail

**Implementation for crw-shield:** Easy
- Add FlareSolverr as optional external service (like SearXNG)
- In the fetcher ladder, when a Cloudflare challenge is detected (response contains
  "Just a moment" or "Performing security verification"), forward the URL to FlareSolverr
- Use returned cookies + HTML directly
- Config: `FLARESOLVERR_URL=http://localhost:8191`

---

### 2. Camoufox (Anti-Detection Firefox)

**What:** A Firefox fork with fingerprint spoofing at the C++ level. Every run gets a
fresh identity drawn from real-world fingerprint datasets.

**Pros:**
- Different browser engine (Firefox) — not subject to Chrome-specific detection
- Fingerprint spoofing at the C++ level (canvas, WebGL, fonts, screen) — harder to
  detect than JS-level patches
- Built for Python automation (Playwright-based)
- Lower resource usage than Chrome-based solutions
- `humanize=True` adds realistic mouse movements

**Cons:**
- Python only — no Rust bindings
- Requires downloading a modified Firefox binary (~100MB)
- Not as mature as nodriver/SeleniumBase for Cloudflare specifically
- Would need a sidecar process (like FlareSolverr but Firefox-based)
- Headless mode less reliable than headed for CF challenges

**Implementation for crw-shield:** Hard
- Would require a Python sidecar service or custom integration
- No Rust bindings — significant architecture change
- Could run as a sidecar similar to FlareSolverr but less proven

---

### 3. Patchright (Patched Playwright)

**What:** A patched version of Playwright that evades bot detection by patching
navigator.webdriver, CDP signatures, and other automation indicators.

**Pros:**
- Drop-in replacement for Playwright (minimal code changes)
- Patches applied at the driver level — harder to detect than JS injection
- Node.js/TypeScript ecosystem — can run as sidecar

**Cons:**
- Node.js dependency (not Rust)
- Still uses Chromium — subject to Chrome-specific detection vectors
- Patches may break with Chromium updates
- Less battle-tested than nodriver for Cloudflare specifically

**Implementation for crw-shield:** Medium
- Would need a Node.js sidecar service
- Or could be used via a custom HTTP API wrapper

---

### 4. Improved CDP Stealth (JS Injection)

**What:** Inject stealth JavaScript before page load to spoof:
- `navigator.webdriver` → false
- Canvas fingerprint randomization
- WebGL vendor/renderer spoofing
- `chrome.runtime` presence
- Permissions API consistency
- `navigator.plugins` / `navigator.languages` consistency
- `window.chrome` object

**Pros:**
- No external dependencies — pure Rust + CDP
- Lightweight — no sidecar containers
- Full control over what gets injected
- Already partially implemented in crw-shield (CDP path exists)

**Cons:**
- Arms race — Cloudflare updates detection faster than patches
- JS-level patches can be detected by checking if the property was overwritten
  (e.g., `navigator.webdriver.toString()` returns different function signatures)
- Does not solve the actual Cloudflare JS challenge — only hides automation signals
- Won't bypass Turnstile CAPTCHA
- Requires `Page.addScriptToEvaluateOnNewDocument` CDP method

**Implementation for crw-shield:** Medium
- Add a stealth JS payload injected via `Page.addScriptToEvaluateOnNewDocument`
- Launch Chromium with `--disable-blink-features=AutomationControlled`
- Add flags: `--disable-features=IsolateOrigins,site-per-process`
- Spoof: navigator.webdriver, canvas, WebGL, chrome.runtime, permissions API
- This is the lowest-hanging fruit for crw-shield's existing architecture

---

### 5. Cookie Jar Persistence

**What:** Save Cloudflare cookies (cf_clearance, __cf_bm, etc.) from successful requests
and reuse them for subsequent requests to the same domain.

**Pros:**
- Dramatically reduces challenge solving frequency
- Works with any approach (CDP, FlareSolverr, etc.)
- Simple to implement in Rust (cookie store on disk or in-memory)
- cf_clearance cookies are valid for hours/days

**Cons:**
- Cookies expire — need periodic refresh
- Tied to IP + User-Agent — if either changes, cookies become invalid
- Doesn't solve the first-time challenge — still need a solver
- Multiple domains need separate cookie jars

**Implementation for crw-shield:** Easy
- Add a persistent cookie store (sled or simple JSON file)
- Key by domain + User-Agent hash
- Before fetching, check if valid CF cookies exist for the domain
- If yes, send them with the request
- If challenge still appears, solve it and store new cookies

---

### 6. 2Captcha / CapSolver (CAPTCHA Solving Services)

**What:** Paid API services that solve CAPTCHAs. Both support Cloudflare Turnstile tokens.

**2Captcha pricing:** ~$1 per 1000 solves (Turnstile)
**CapSolver pricing:** ~$1.20 per 1000 solves (Turnstile, < 3s solve time)

**API flow:**
1. Detect Turnstile challenge on page
2. Extract `sitekey` from the Turnstile widget
3. Submit to 2Captcha/CapSolver API with the page URL
4. Poll for the token solution (3-15 seconds)
5. Inject token into the Turnstile response field
6. Submit the form / reload the page

**Pros:**
- Handles Turnstile CAPTCHAs (the hardest challenge type)
- Affordable for low-volume scraping
- Reliable — these services have 99%+ success rates
- API is simple (HTTP POST + poll)

**Cons:**
- Paid service — cost scales with volume
- Requires API key configuration
- Adds latency (3-15s per CAPTCHA)
- Only solves Turnstile — doesn't help with JS challenges or behavioral analysis
- Privacy concern: sending URLs to third-party service

**Implementation for crw-shield:** Medium
- Add optional CAPTCHA solver configuration (2CAPTCHA_API_KEY or CAPSOLVER_API_KEY)
- When Turnstile detected on page, extract sitekey and submit to solver
- Inject returned token and reload page
- Fallback to FlareSolverr if CAPTCHA solving fails

---

### 7. Nodriver / Zendriver (Python CDP Frameworks)

**What:** Successors to undetected-chromedriver. Pure CDP — no WebDriver protocol,
which eliminates WebDriver-specific detection vectors.

**Nodriver:** Built from scratch, uses direct CDP, patches navigator.webdriver at
the driver level. Created by the author of undetected-chromedriver.

**Zendriver:** Fork of nodriver with more active development.

**Pros:**
- Most effective Chrome-based anti-detection as of 2026
- No WebDriver protocol traces at all
- Direct CDP access — fine-grained control
- Active development communities

**Cons:**
- Python only — no Rust bindings
- Would need a sidecar process (like FlareSolverr, which already uses undetected-chromedriver)
- FlareSolverr already uses undetected-chromedriver under the hood — nodriver is an
  incremental improvement, not a paradigm shift

**Implementation for crw-shield:** Hard
- Would need to build a custom sidecar using nodriver
- FlareSolverr is easier and already exists

---

### 8. Browser Fingerprint Randomization

**What:** Rotate browser fingerprints (screen resolution, OS, browser version, fonts,
canvas hash, WebGL renderer) from real-user datasets across requests.

**Pros:**
- Prevents fingerprint-based blocking across requests
- Complements any other approach
- Can be done at the CDP level (emulation)

**Cons:**
- Needs a fingerprint dataset (real-world fingerprints)
- Complex to implement correctly (many vectors to cover)
- Doesn't solve challenges — only prevents detection
- Overkill for most use cases

**Implementation for crw-shield:** Hard
- Generate random fingerprints from real-world distributions
- Set via CDP: `Emulation.setDeviceMetricsOverride`, canvas/WebGL spoofing via JS
- Best used in combination with cookie jar persistence

---

## Recommendation for crw-shield

### Phase 1 — Immediate (Easy, High Impact)

**1. Improve CDP Stealth (Approach 4) + Chromium Flags**
- Inject stealth JS via `Page.addScriptToEvaluateOnNewDocument`
- Add Chromium flags: `--disable-blink-features=AutomationControlled`
- Spoof: navigator.webdriver, canvas, WebGL, chrome.runtime
- **Difficulty:** Easy-Medium
- **Impact:** May solve JS challenges without external help for some sites
- **Time:** 1-2 days

**2. Cookie Jar Persistence (Approach 5)**
- Save CF cookies per domain, reuse across requests
- **Difficulty:** Easy
- **Impact:** Reduces challenge frequency by 90%+ after first solve
- **Time:** 1 day

### Phase 2 — Short Term (Medium, Reliable)

**3. FlareSolverr Integration (Approach 1)**
- Add as optional external service (like SearXNG)
- When CDP detects a CF challenge it can't bypass, delegate to FlareSolverr
- Use FlareSolverr's session management for cookie reuse
- Store returned cookies in the cookie jar for future non-FlareSolverr requests
- **Difficulty:** Easy (Docker container + HTTP client)
- **Impact:** Solves most CF challenges reliably
- **Time:** 2-3 days

### Phase 3 — Optional Enhancements

**4. 2Captcha/CapSolver for Turnstile (Approach 6)**
- Only if Turnstile CAPTCHAs are encountered
- Optional config, off by default
- **Difficulty:** Medium
- **Time:** 1-2 days

### Why Not Camoufox/Nodriver/Patchright?

These are all Python/Node.js based and would require a sidecar process. FlareSolverr
already serves this role and is more proven. If FlareSolverr proves insufficient,
considering nodriver as a replacement for FlareSolverr's undetected-chromedriver
backend would be the next step — but that's FlareSolverr's problem, not crw-shield's.

### Architecture

```
crw-shield (Rust)
├── HTTP fetcher (reqwest) → fast path, no CF
├── CDP fetcher (Chromium + stealth JS) → handles JS challenges
│   ├── Cookie jar (persistent CF cookies per domain)
│   └── Stealth JS injection (navigator.webdriver, canvas, etc.)
└── FlareSolverr fallback (optional, external)
    └── When CDP fails → delegate URL to FlareSolverr → get HTML + cookies
        → store cookies in jar for future requests
```

This layered approach means most sites never need FlareSolverr (stealth + cookies
handle them), and FlareSolverr is only invoked for hard challenges. The cookie jar
ensures each domain only needs FlareSolverr once per cookie lifetime.