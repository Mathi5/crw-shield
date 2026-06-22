# Recherche Firecrawl markdown + Taxonomy anti-bot

Recherche menée le 19 juin 2026. Sources concrètes citées avec URL + extrait court.

---

## Sujet 1 — Firecrawl markdown generation : à quel point est-il agressif ?

### 1. Architecture de l'extraction Firecrawl (le pipeline réel)

**TL;DR :** Firecrawl **n'utilise PAS Mozilla Readability ni cheerio pour son extracteur principal**. Il utilise un pipeline en 3 niveaux :

1. **Niveau 1 (principal, depuis ~avril 2026)** : un **microservice Go** basé sur un fork de `JohannesKaufmann/html-to-markdown`, appelé via `HTML_TO_MARKDOWN_SERVICE_URL` ou via une lib native (`HTML_TO_MARKDOWN_PATH`) chargée avec **koffi** (FFI).
2. **Niveau 2 (natif Rust, nouveau)** : `@mendable/firecrawl-rs` qui fait un `postProcessMarkdown` sur le résultat.
3. **Niveau 3 (fallback)** : `turndown` + `joplin-turndown-plugin-gfm` (HTML → MD avec règles custom).

Sources :
- Le code source principal est `apps/api/src/lib/html-to-markdown.ts` dans le repo Firecrawl. Extrait : *"Try HTTP service first if enabled — Try Go parser if config.USE_GO_MARKDOWN_PARSER — Fallback to TurndownService if Go parser fails or is not enabled"*. ([raw.githubusercontent.com/firecrawl/firecrawl/main/apps/api/src/lib/html-to-markdown.ts](https://raw.githubusercontent.com/firecrawl/firecrawl/main/apps/api/src/lib/html-to-markdown.ts))
- Le package.json confirme les deps clés : `cheerio ^1.0.0-rc.12`, `jsdom ^29.1.1`, `turndown ^7.1.3`, `marked ^14.1.2`, `joplin-turndown-plugin-gfm ^1.0.12`. ([raw.githubusercontent.com/firecrawl/firecrawl/main/apps/api/package.json](https://raw.githubusercontent.com/firecrawl/firecrawl/main/apps/api/package.json))
- Le fork Go du convertisseur HTML→MD : [github.com/firecrawl/html-to-markdown](https://github.com/firecrawl/html-to-markdown) — *"A Go library that converts HTML into Markdown using the goquery HTML Parser"*. C'est lui qui fait le gros du boulot.
- `@mendable/firecrawl-rs` est appelé après la conversion : *"markdownContent = await postProcessMarkdown(markdownContent)"*. ([html-to-markdown.ts](https://raw.githubusercontent.com/firecrawl/firecrawl/main/apps/api/src/lib/html-to-markdown.ts))
- Un **nouvel extracteur Rust dédié** est sorti en open source en mai 2026 : [github.com/firecrawl/html-extractor](https://github.com/firecrawl/html-extractor) — *"Fast HTML main-content extractor in Rust with Node bindings. Page-type-aware, outputs clean markdown"*. Il est marqué *"inspired by Python's trafilatura. Implementation is from scratch"*. **C'est probablement l'avenir de leur pipeline.**

### 2. Stratégie de sélection du "main content"

**Dans Firecrawl :**
- Le **choix "main content"** n'est PAS fait par Readability dans le pipeline actuel. C'est le fork `html-to-markdown` (Go) qui gère la conversion avec son propre système de règles et `Remove()` / `Keep()`.
- Le **nouveau `firecrawl/html-extractor`** (Rust) par contre décrit son pipeline en 5 stages explicites :
  ```
  Stage 1 — pre-clean: drop <script>/<style>/<head>/comments
  Stage 2 — page-type classification: pick a scoring profile per type
  Stage 3 — score + select main subtree: text density / link density / tag weights / class hints / position / parent chain
  Stage 4 — fallback chain if Stage 3 degraded: justext-style, readability-style, raw text
  Stage 5 — post-clean + markdown render
  ```
  Extrait : *"Algorithm inspired by Python's trafilatura. Implementation is from scratch."* ([github.com/firecrawl/html-extractor](https://github.com/firecrawl/html-extractor))

- Le pipeline principal `scrapeURL` a une **option `onlyMainContent`** qui contrôle le comportement :
  ```typescript
  // Extrait de deriveMarkdownFromHTML (pipeline Firecrawl)
  const fallbackMeta = {
    ...meta,
    options: { ...meta.options, onlyMainContent: false },
  };
  document = await deriveHTMLFromRawHTML(fallbackMeta, document);
  ```
  → *"If `onlyMainContent` results in empty markdown, retries with `onlyMainContent: false`"*. ([transformers/index.ts](https://raw.githubusercontent.com/firecrawl/firecrawl/main/apps/api/src/scraper/scrapeURL/transformers/index.ts))

### 3. Filtres et options user-facing

L'API publique `/scrape` expose ces options pertinentes (cf. [docs.firecrawl.dev/api-reference/endpoint/scrape](https://docs.firecrawl.dev/api-reference/endpoint/scrape)) :

| Option | Effet |
|---|---|
| `formats: ["markdown"]` | Demande la sortie markdown |
| `onlyMainContent: true` | Tente de ne garder que le contenu principal (avec fallback si vide) |
| `onlyCleanContent: false` | Active le nettoyage agressif via LLM |
| `includeTags: ["h1", "p", "a", ".main-content"]` | Force l'inclusion de tags |
| `excludeTags: ["#ad", "#footer"]` | Force l'exclusion de tags |
| `blockAds: true` | Tente de bloquer les ads |
| `removeBase64Images: true` | Retire les images base64 |
| `parsers: ["pdf"]` | Active LlamaParse pour PDFs |

**Le transformeur `performRemoveBase64Images`** existe ([transformers/removeBase64Images.ts](https://github.com/firecrawl/firecrawl/blob/main/apps/api/src/scraper/scrapeURL/transformers/removeBase64Images.ts)).

### 4. Issues GitHub mentionnant l'agressivité

Plusieurs issues documentent des cas où Firecrawl **coupe trop** ou ne respecte pas les exclusions :

- **#1564** *"onlyImportantContent / Tag exclusions appear to have no effect"* — Closed (Not planned). L'utilisateur signale que `excludeTags` ne marche pas comme attendu. ([issue #1564](https://github.com/firecrawl/firecrawl/issues/1564))
- **#1518** *"Option to include header/footer once when using onlyMainContent"* — Closed (Not planned). Demande légitime que Firecrawl ignore — l'auteur veut garder header/footer même avec `onlyMainContent`. ([issue #1518](https://github.com/firecrawl/firecrawl/issues/1518))
- **#748** *"Improve extract only main content"* — Not planned. Demande d'amélioration de la sélection du main content. ([issue #748](https://github.com/firecrawl/firecrawl/issues/748))
- **#1360** *"h1/h2 titles missing # marks when converting Markdown"* — Closed. Bug d'agressivité : les `<h1>` étaient convertis en plain text au lieu de `# title`. ([issue #1360](https://github.com/firecrawl/firecrawl/issues/1360))
- **#1297** *"Markdown is empty, HTML works fine"* — Closed. Cas où l'extraction markdown rend vide alors que le HTML contient du contenu. ([issue #1297](https://github.com/firecrawl/firecrawl/issues/1297))
- **#1** (le tout premier ticket) *"Strip non-content tags, headers, footers"* — Closed. La motivation originelle du projet.

### 5. Comparaison avec notre extracteur

| Aspect | Notre `content.rs` (1074 lignes) | Firecrawl (avant `html-extractor`) | Firecrawl `html-extractor` (mai 2026) |
|---|---|---|---|
| **Algorithme** | Custom : `is_noise_element` + scoring `readability_score` maison | Fork Go de `html-to-markdown` (règles fixes par tag) | 5-stage pipeline, scoring type trafilatura |
| **Base** | sélecteurs CSS (70+ noise patterns) + scoring custom | Règles hardcodées par tag HTML | Page-type classifier + densité de texte + link density |
| **ML** | Non | Non (mais LLM en option `onlyCleanContent`) | Non (extraction heuristique pure) |
| **Tolère hashed classes** | **Non** (point faible) | Partiellement | **Oui** (pas de dépendance aux classes CSS) |
| **Préserve `<main>`/`<article>`** | Oui (notre `is_noise_element` lignes 231-239) | Dépend des règles Go | Oui (détection par structure, pas par classe) |
| **Multi-page-type** | Non (assume article) | Non | **Oui** (article, product, listing, forum, doc) |
| **Retour d'info** | Score seulement | `markdown` brut | `markdown + page_type + extraction_quality` |

### 6. Risques identifiés pour crw-shield (3-5)

**Risque 1 — Trop agressif sur des pages non-articles.**
Notre scoring assume que c'est un article. Sur des pages produit, listing, doc — on coupe du contenu utile. Firecrawl a reconnu ce problème et a construit `html-extractor` justement pour le résoudre. ([html-extractor README](https://github.com/firecrawl/html-extractor))

**Risque 2 — Brittle sur hashed classnames.**
Nos 70+ noise patterns sont des regex contre des mots (sidebar, ad, footer, cookie-banner…). Les sites modernes hashent leurs classes (`css-1x2y3z4`). Notre extracteur va laisser passer du noise. → Solution : passer au scoring par densité, comme `html-extractor` ou Mozilla Readability.

**Risque 3 — Pas de fallback "main content vide → full content".**
Firecrawl a ce comportement explicite : *"If `onlyMainContent` results in empty markdown, retries with `onlyMainContent: false`"*. Si on n'a pas ça, on peut renvoyer du vide sur des sites que Mozilla Readability aurait su gérer en mode permissif.

**Risque 4 — Aucun retour diagnostique.**
On retourne juste un markdown. Firecrawl `html-extractor` retourne `markdown + page_type + extraction_quality (0.0-1.0)`. C'est ce qui permet au scrapeur de **savoir** s'il doit escalader (CDP) ou non.

**Risque 5 — Pas de support `includeTags`/`excludeTags`.**
Firecrawl expose ça en option user-facing. Sans ça, les utilisateurs n'ont aucun levier pour corriger un cas particulier. ([scrape API ref](https://docs.firecrawl.dev/api-reference/endpoint/scrape))

### 7. Recommandation d'approche

Court terme (1-2 jours) :
1. Ajouter une **étape de fallback** : si `extract_main_with_scoring` retourne < N chars, réessayer avec un mode "all content" (notre `strip_unwanted` mais sans scoring). Ça matche le comportement Firecrawl.
2. Ajouter un champ `extraction_quality: f32` au retour de l'extracteur, basé sur le score final vs seuil. Permet à `FetchLadder` de décider.
3. Exposer `include_tags` / `exclude_tags` au niveau API.

Moyen terme :
- Porter `firecrawl/html-extractor` en Rust (licence Apache-2.0) ou s'en inspirer fortement. L'algo trafilatura est bien plus robuste que nos noise patterns.

---

## Sujet 2 — Taxonomy des situations anti-bot / JS-only

### 1. État de l'art : `is-antibot` (microlinkhq)

Le projet de référence est **[microlinkhq/is-antibot](https://github.com/microlinkhq/is-antibot)** (38 stars, MIT, ~30 providers couverts). Extrait : *"Detect anti-bot protection from 20+ providers — CloudFlare, Akamai, DataDome, PerimeterX, Kasada, Imperva, reCAPTCHA, hCaptcha, Turnstile, and more."*

**Architecture clé** (d'après [src/index.js](https://raw.githubusercontent.com/microlinkhq/is-antibot/master/src/index.js)) :
- 5 types de détection : `headers`, `cookies`, `html`, `url`, `status_code`.
- Un fichier unique `providers.json` décrit tous les providers sous forme déclarative.
- Les `DETECTION_COMPILERS` compilent chaque règle en un test optimisé.

### 2. Signatures extraites de `is-antibot/src/providers.json`

Source : [raw.githubusercontent.com/microlinkhq/is-antibot/master/src/providers.json](https://raw.githubusercontent.com/microlinkhq/is-antibot/master/src/providers.json)

| Catégorie | Cookies (smoking gun) | Headers (smoking gun) | HTML/URL (soft signals) |
|---|---|---|---|
| **Cloudflare** | `cf_clearance=` | `cf-mitigated: challenge` | `Server: cloudflare`, `cf-ray` |
| **Vercel** | — | `x-vercel-mitigated: challenge` | — |
| **Akamai Bot Manager** | `_abck=`, `bm_sz`, `ak_bmsc` | `Server: AkamaiGHost` | Inlined `sensor.js`, body *"Pardon Our Interruption"* |
| **AWS WAF** | `aws-waf-token=` | `x-amzn-waf-action` | `aws-waf` string |
| **Imperva / Incapsula** | `incap_ses_`, `visid_incap_`, `reese84=` | `x-cdn: Incapsula`, `x-iinfo` | `incapsula`, `imperva` |
| **Reblaze** | `rbzid=`, `rbzsessionid=` | — | `reblaze` |
| **DataDome** | `datadome=` | `x-dd-b: 1\|2`, `x-datadome-cid`, `x-datadome` (≠ `protected`) | `geo.captcha-delivery.com` iframe |
| **PerimeterX** | `_px3=`, `_pxhd=` | `x-px-authorization` | `window._pxAppId`, `pxInit` |
| **Shape Security** | `TS*`, `reese84` | Headers regex `^x-[a-z0-9]{8}-[abcdfz]$` | `shapesecurity` |
| **Kasada** | `x-kpsdk-ct`, `x-kpsdk-cd` | `x-kasada` | `__kasada`, `kasada.js` |
| **reCAPTCHA** | — | — | `grecaptcha.render(`, `__grecaptcha_cfg`, `g-recaptcha` |
| **hCaptcha** | — | — | `hcaptcha.com`, `h-captcha` |
| **Turnstile (CF)** | — | — | `cf-turnstile`, `challenges.cloudflare.com/turnstile` |
| **FunCaptcha** | — | — | `arkoselabs.com`, `funcaptcha` |
| **LinkedIn** | — | — | status `999` |
| **Reddit** | — | — | status `403`/`429` sur `reddit.com`, *"blocked by network security"* |
| **Amazon** | — | `x-cache: Error from cloudfront` | `csm-captcha-instrumentation` |

### 3. Cheatsheet Scrappey (validation externe)

Source : [scrappey.com/qa/anti-bot/anti-bot-vendor-detection-cheatsheet](https://scrappey.com/qa/anti-bot/anti-bot-vendor-detection-cheatsheet) (mis à jour 2026-05-31).

Citations courtes :
- *"Vendors covered: Akamai, Cloudflare, DataDome, PerimeterX, Kasada, F5 Shape. Detection time: A single HTTP response. Fastest signal: Set-Cookie names. Most ambiguous: Cloudflare — ~20% of all sites, often with no Bot Management enabled."*
- **Workflow 4-step** recommandé : (1) Cookies d'abord, (2) Server header, (3) `<script src>` tags, (4) Block body text.
- **Status codes typiques** : Akamai `412 Pardon Our Interruption`, Cloudflare error `1015` sur rate limit, Kasada silent `403/429` sans UI.

### 4. `cloudscraper` — détection de chaque variante Cloudflare

Source : [github.com/VeNoMouS/cloudscraper/blob/master/cloudscraper/cloudflare.py](https://github.com/VeNoMouS/cloudscraper/blob/master/cloudscraper/cloudflare.py)

5 fonctions de détection spécifiques :
```python
is_IUAM_Challenge(resp)        # status 429/503, /cdn-cgi/images/trace/jsch/, __cf_chl_f_tk
is_New_IUAM_Challenge(resp)    # orchestrate/jsch/v1
is_Captcha_Challenge(resp)     # status 403, /cdn-cgi/images/trace/(captcha|managed)/
is_New_Captcha_Challenge(resp) # orchestrate/captcha ou managed/v1
is_Firewall_Blocked(resp)      # status 403, <span class="cf-error-code">1020</span>
```

C'est un bon modèle pour notre `detect_challenge` actuel : remplacer la détection unique par un enum + une priorité.

### 5. Cloudscraper — orchestration du ladder

`cloudscraper` distingue 4 types de challenges Cloudflare et a un `solveDepth=3` max avant `CloudflareLoopProtection`. ([cloudscraper/__init__.py](https://raw.githubusercontent.com/VeNoMouS/cloudscraper/master/cloudscraper/__init__.py))

### 6. `antibot-detector` (mihneamanolache)

Repo : [github.com/mihneamanolache/antibot-detector](https://github.com/mihneamanolache/antibot-detector) — basé sur la logique de **Wappalyzer** (technologies detection). Format de sortie structuré :
```ts
{ antiBot: [{ name, version, confidence, pattern }], other: [...] }
```

Bon pour inspiration sur la structure de retour (avec `confidence`).

### 7. Curl_cffi — TLS fingerprinting comme signal

Source : [github.com/lexiforest/curl_cffi](https://github.com/lexiforest/curl_cffi) — *"curl_cffi can impersonate browsers' TLS/JA3 and HTTP/2 fingerprints. If you are blocked by some website for no obvious reason, you can give curl_cffi a try."*

→ Le TLS fingerprint est une **catégorie M** (bot detection from headers/handshake) — on ne peut pas la détecter depuis la réponse HTTP, mais on peut savoir si on l'a subie (parce qu'on reçoit un challenge alors qu'on n'a même pas encore atteint le serveur).

### 8. État actuel de notre `challenge_detect.rs`

Notre code actuel ([/home/moi/projects/hermes/crw-shield/crates/antibot/src/challenge_detect.rs](file:///home/moi/projects/hermes/crw-shield/crates/antibot/src/challenge_detect.rs)) :

- ✅ Détecte 5 providers (Cloudflare, hCaptcha, reCAPTCHA, PerimeterX, DataDome) via `detect_challenge` qui retourne `Option<String>`.
- ✅ Détecte les pages "vides / bloquées" via `detect_empty_or_blocked` avec heuristique sur la taille + 20 BLOCK_PHRASES + comptage de `<script>`.
- ❌ Binaire (blocked / not blocked).
- ❌ Pas de distinction entre Cloudflare IUAM vs Turnstile vs WAF.
- ❌ Pas de détection de Akamai, Imperva, Kasada, AWS WAF, etc.
- ❌ Pas de signalisation `retry_after`, `requires_javascript`, `confidence`.

### 9. Recommandation : enum `FetchSituation` proposé

D'après toutes les sources ci-dessus, voici la taxonomy que je recommande pour `crates/antibot/src/`:

```rust
pub enum FetchSituation {
    /// 200 OK, HTML bien formé, contenu utile > 5000 chars
    CleanSuccess { html_len: usize },
    /// 200 OK mais HTML < 2000 chars, structure SPA, pas de <article>/<main>
    JsOnly { html_len: usize, script_count: usize },
    /// Cloudflare interstitial "Just a moment..." ou cf-mitigated
    CloudflareChallenge { variant: CfVariant },  // Iuam, Turnstile, Managed, Waf
    /// DataDome — captcha-delivery, datadome cookie, x-dd-b
    DataDomeCaptcha,
    /// Akamai — _abck, sensor.js, "Pardon Our Interruption"
    AkamaiBotManager { status: u16 },
    /// Imperva/Incapsula — incap_ses_, _Incapsula_Resource
    ImpervaIncapsula,
    /// PerimeterX/HUMAN — _px3, press & hold
    PerimeterX,
    /// reCAPTCHA ou hCaptcha ou Turnstile
    Captcha { provider: CaptchaProvider },
    /// Status 451 ou "not available in your country"
    GeoBlocked { status: u16 },
    /// Status 429 + Retry-After header
    RateLimited { retry_after_secs: Option<u64> },
    /// Login wall — "Sign in", "Log in", WWW-Authenticate
    LoginWall,
    /// Status 200 mais contenu dit "404" ou "not found"
    SoftNotFound { signals: Vec<String> },
    /// Status 403 + "Attention Required" ou ray id
    WafBlocked { provider: String, status: u16 },
    /// Status 4xx/5xx non classifié
    HttpError { status: u16, body_excerpt: String },
    /// Inconnue / pas classifié
    Unknown { status: u16, html_len: usize },
}

pub struct SituationReport {
    pub situation: FetchSituation,
    pub confidence: f32,             // 0.0 - 1.0
    pub requires_javascript: bool,   // true pour JsOnly + challenges JS
    pub retry_after_seconds: Option<u64>,
    pub detection_signals: Vec<String>,  // ["cf_clearance", "cf-mitigated: challenge"]
}
```

### 10. Champs utiles pour le `FetchLadder`

D'après la signature d'`is-antibot` ([index.js](https://raw.githubusercontent.com/microlinkhq/is-antibot/master/src/index.js)) et `cloudscraper`, les champs minimaux sont :

| Champ | Pourquoi | Source inspiration |
|---|---|---|
| `situation: FetchSituation` | Décision principale | is-antibot return |
| `confidence: f32` | Permet à l'appelant d'arbitrer | antibot-detector |
| `requires_javascript: bool` | true si la ladder doit escalader vers CDP | Détection SPA shell (notre code) |
| `retry_after_seconds: Option<u64>` | Header `Retry-After` parsing | HTTP standard + cloudscraper |
| `detection_signals: Vec<String>` | Debug + logs | is-antibot DETECTION type |
| `provider: String` | Qui bloque (utile pour adapter les retries) | is-antibot |

### 11. Sources canoniques (≥ 5 références)

1. **[github.com/firecrawl/html-extractor](https://github.com/firecrawl/html-extractor)** — Le nouvel extracteur Rust de Firecrawl, page-type-aware, licence Apache-2.0. *Référence directe pour l'évolution de notre `extract/src/content.rs`.*
2. **[github.com/microlinkhq/is-antibot](https://github.com/microlinkhq/is-antibot)** + [src/providers.json](https://raw.githubusercontent.com/microlinkhq/is-antibot/master/src/providers.json) — Le plus complet (~30 providers, JSON déclaratif). *Inspiration directe pour la structure `providers.json` ou `providers.toml` côté Rust.*
3. **[github.com/VeNoMouS/cloudscraper](https://github.com/VeNoMouS/cloudscraper)** + [cloudscraper/cloudflare.py](https://github.com/VeNoMouS/cloudscraper/blob/master/cloudscraper/cloudflare.py) — Détection fine des variantes Cloudflare (IUAM, Turnstile, Captcha, WAF). *Référence pour la discrimination Cloudflare.*
4. **[scrappey.com/qa/anti-bot/anti-bot-vendor-detection-cheatsheet](https://scrappey.com/qa/anti-bot/anti-bot-vendor-detection-cheatsheet)** — Cheatsheet 2026 avec workflow 4-step et cookie state machines.
5. **[github.com/mihneamanolache/antibot-detector](https://github.com/mihneamanolache/antibot-detector)** — Basé sur Wappalyzer, structure de sortie avec `confidence`.
6. **[github.com/lexiforest/curl_cffi](https://github.com/lexiforest/curl_cffi)** — TLS/JA3 fingerprinting, important pour la catégorie M (bot detection from headers).
7. **[github.com/firecrawl/firecrawl](https://github.com/firecrawl/firecrawl)** — Source directe du pipeline Firecrawl : [apps/api/src/lib/html-to-markdown.ts](https://raw.githubusercontent.com/firecrawl/firecrawl/main/apps/api/src/lib/html-to-markdown.ts) et [apps/api/src/scraper/scrapeURL/transformers/index.ts](https://raw.githubusercontent.com/firecrawl/firecrawl/main/apps/api/src/scraper/scrapeURL/transformers/index.ts).
8. **[github.com/firecrawl/html-to-markdown](https://github.com/firecrawl/html-to-markdown)** — Fork Go de `JohannesKaufmann/html-to-markdown`, c'est ce qui fait la conversion HTML→MD.
9. **[mozilla/readability](https://github.com/mozilla/readability)** — Le standard historique, bien plus robuste que notre scoring custom mais plus conservateur que Firecrawl.

### 12. Approche concrète recommandée

Court terme (1-2 jours) :
1. **Créer `crates/antibot/src/situation.rs`** avec l'enum `FetchSituation` ci-dessus et une fonction `classify(status, headers, html, url) -> SituationReport`.
2. **Convertir nos BLOCK_PHRASES et signatures `detect_challenge`** en un fichier déclaratif TOML (`providers.toml`) : pattern + category + priority.
3. **Faire vivre `FetchLadder`** avec `SituationReport` au lieu de `bool` blocked.

Moyen terme :
- Étudier l'intégration d'un vrai client **TLS-impersonating** (type curl_cffi mais en Rust : [`wreq`](https://github.com/0x676e67/wreq), [`rquest`](https://github.com/0x676e67/rquest)).
- Implémenter la **page-type classification** dans l'extracteur pour s'aligner sur `firecrawl/html-extractor`.

---

## Notes méthodologiques

- Le repo `https://github.com/Top-Ant-SEO/WAF-Detection` mentionné dans la requête **n'existe plus** (404 à l'extraction).
- Le code source complet de `firecrawl/firecrawl` est en TypeScript + Go (microservice) + Rust (natif NAPI). Il a été refondu plusieurs fois en 2024-2026 (PR #714 refactor en `scrapeURL`, PR #3818 product service, etc.).
- L'extracteur Mozilla Readability n'est **PAS utilisé** dans le pipeline Firecrawl actuel — c'est un mythe. L'équipe Firecrawl a construit ses propres extracteurs (`html-to-markdown` Go + `html-extractor` Rust).