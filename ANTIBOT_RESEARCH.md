# Anti-bot bypass research — CortexScout, Firecrawl, Datadome/Akamai

Date: 2026-06-19  
Auteur: crw-shield subagent — Hermes Agent  
Cible: améliorations à implémenter dans `crates/antibot` et `crates/fetch`.

---

## 0. Baseline mesurée (avant changement)

Lancé le 19 juin 2026 contre `crw-srv` configuré avec FlareSolverr activé, CDP activé,
Chromium `/usr/bin/chromium`. Résultats sur 1 essai par URL :

| URL                                       | HTTP | CDP | Résultat                          | Chars | Temps |
|-------------------------------------------|------|-----|------------------------------------|-------|-------|
| `https://www.amazon.fr/gp/bestsellers/`   | ✅   | ✅  | 200 — page complète                | 42459 | 2.3 s |
| `https://www.amazon.fr/` (homepage)        | ❌   | —   | 202 — anti-bot, pas d'escalade     |     0 | 0.05 s|
| `https://www.amazon.fr/dp/<ASIN>` (404)   | ❌   | —   | 200 (404 Amazon) — pas le vrai pb  |   597 | 0.4 s |
| `https://www.leboncoin.fr`                | ❓   | ✅  | 200 — page partielle               |  8761 | < 5 s |
| `https://stackoverflow.com/`              | ✅   | ✅  | 200 — complet                       | 18789 | < 3 s |

Constats :

- **Amazon** : la home renvoie 0 char (statusCode 202), pas de Markdown. Le bestsellers
  marche, donc c'est **uniquement** la home et la recherche qui sont bloquées (le cookie
  `x-amz-rid` se déclenche). Le CDP ne s'escalade pas car la ladder considère la réponse
  HTTP comme « non-challenge » (200 ≠ 403/429, pas de HTML signature).
- **Leboncoin** : la home renvoie du contenu, mais **DataDome est actif** dès qu'on
  navigue dans des pages annonce. Notre CDP stealth est correct (navigator.webdriver,
  UA, plugins, WebGL, canvas, audio) mais le `webdriver: undefined` patch est encore
  détectable (toString natif, canvas hash stable).
- **StackOverflow** : 18K chars, OK.

---

## 1. CortexScout — ce qu'ils font

Source : <https://agenthubstack.dev/tools/cortex-scout> (projet cortex-works/cortex-scout,
66 stars, MIT, mis à jour 2026).

Architecture en 4 niveaux progressifs :
1. **Native retrieval** (HTTP léger) — 1er essai, rapide.
2. **Chromium CDP rendering** — fallback quand HTTP échoue.
3. **Stateful E2E testing** — réutilise une session CDP entre requêtes.
4. **HITL workflows** — escalade humaine pour CAPTCHAs.

Caractéristiques :
- **Proxy rotation** + retries aware du type de bloc.
- **Self-hostable** binary Rust, compatible MCP stdio + HTTP.
- **Pas de patch CDP stealth custom** documenté publiquement — ils s'appuient sur la
  combinaison UA rotation + navigateur réel + retries.

**Ce qu'on a déjà en commun** : ladder HTTP→CDP, UA rotation, BrowserProfile, delays
jitterisés. **Ce qui nous manque** : stateful E2E (cookies persistés entre requêtes),
HITL endpoint, et un mode « escalade automatique vers la home Amazon si page produit
renvoie 404-Amazon ».

---

## 2. Firecrawl — comment ils gèrent l'e-commerce

Source : docs Firecrawl (`developer-guides/common-sites/amazon`).

Firecrawl **ne révèle pas** ses techniques anti-bot publiquement. Côté code/usage, on
voit qu'ils exposent seulement :
- `formats: ['markdown']` + `onlyMainContent: true`
- JSON mode avec Zod schema
- Map / Crawl / Batch / Search
- Paramètres `proxy: 'basic' | 'stealth' | 'auto'` non documentés publiquement

Hypothèse (cohérente avec leur infra cloud) : ils utilisent un **pool de navigateurs
managés** derrière un reverse proxy, ce qui résout le problème IP reputation que
crw-shield ne peut pas régler côté serveur. **Leurs `proxy: stealth` masque les TLS
fingerprints par session**.

**Leçon applicable à crw-shield** :
- Le format de réponse doit **savoir ignorer** les pages 404/spinner pour détecter un
  vrai blocage (Amazon renvoie 200 + 404 HTML sur produits inexistants).
- L'extraction de pages e-commerce (Leboncoin) doit cibler le `__NEXT_DATA__` JSON
  embarqué dans le HTML plutôt que de parser le DOM rendu.

---

## 3. Industrie 2026 — techniques anti-bot

### 3.1 TLS fingerprint (JA3 / JA4+)

Référence : <https://scrapfly.io/blog/posts/how-to-bypass-anti-bot-protection>.

> « No bypass technique works across all eight vendors. **DataDome uses ML scoring,
> Kasada uses active environment interrogation, and Akamai validates TLS JA4+
> fingerprint + behavioural telemetry**. »

Mesure concrète (Medium 2026) :
> « Using **curl_cffi** (which perfectly mimics browser TLS fingerprints): **94%
> success rate** on Amazon. That's a **47x improvement** from a single technical change. »

**Bilan pour crw-shield** : la plus haute ROI. Le passage de `reqwest + rustls` à un
client HTTP qui imite Chrome 131 (TLS client hello, cipher suites, extensions, ALPN,
HTTP/2 SETTINGS + WINDOW_UPDATE + HPACK) débloque Amazon home et probablement
l'IP-reputation layer de DataDome.

Outils Rust disponibles :
- `boring-tls` (fork de rustls) — pas de controle bas-niveau du ClientHello.
- `curl-rustls` binding — possible mais lourd à compiler.
- **Piste réaliste** : réécrire le `HttpFetcher` pour qu'il utilise un profile « Chrome
  131 » au niveau des headers HTTP/2 (`HeaderOrder` déjà géré par reqwest) et
  intégrer un mécanisme **de retry avec un BrowserProfile différent** (UA + viewport +
  sec-ch-ua) à chaque tentative. Le vrai gain TLS fingerprint viendra plus tard via
  un client custom ou un binding `boring` patché.

### 3.2 Browser fingerprint (navigator, canvas, WebGL, audio)

C'est **ce qu'on fait déjà** dans `cdp_stealth.rs`. Le script couvre :
- navigator.webdriver (avec toString natif)
- navigator.languages / plugins / userAgentData
- chrome.csi / chrome.loadTimes / chrome.app
- WebGL vendor/renderer (Intel Iris)
- Canvas getImageData + toDataURL (bruit de 256 pixels)
- AudioContext getByteFrequencyData (perturbFreq deterministe)
- Screen 1280x800, hardwareConcurrency=8, deviceMemory=8
- Permissions API (notifications), automation markers

**Gaps identifiés** :
- Le patch `webdriver` met `undefined` mais le test avancé inspecte la présence de
  l'API getter (real Chrome l'a en lecture, on a un getter vide). Patchright documente
  qu'il faut **laisser `navigator.webdriver` `false`** au lieu de `undefined` (ou
  carrément supprimer la propriété) pour matcher le comportement de Chrome réel.
- Le `chrome.csi.startE` est recalculé au reload — risque de fingerprint drift.
- Le canvas noise est **stable** entre les requêtes (mêmes 256 pixels perturbés).
  DataDome croise les hashes entre sessions. → il faut un seed aléatoire par session.
- Le `dd` cookie (DataDome) **n'est pas posé** par notre pipeline : on n'extrait pas
  les cookies de la réponse CDP pour les réutiliser côté HTTP.
- Pas de **WebRTC IP leak** patch (DataDome le lit).

### 3.3 Behavioural (souris, scroll, timing)

CortexScout / Patchright simulent :
- Mouvements de souris en courbe de Bézier entre 2 points.
- Scroll incrémental avec pauses.
- Délais aléatoires 200-800 ms entre actions.

**Notre CDP** (`cdp.rs::apply_action`) ne fait que des `window.scrollBy` directs. Pas
de mouse.move, pas de wait_for_selector, pas de timing humain.

### 3.4 Cookie persistence

C'est le **plus gros gap** : aucun de nos fetchers ne réutilise les cookies. Pour
Leboncoin (DataDome) et Amazon (cookie `x-amz-rid` + `session-id`), la première
requête pose les cookies et les suivantes doivent les réutiliser. Patchright fait ça
nativement.

### 3.5 HTTP/2 fingerprinting

Au-delà de TLS, le SETTINGS frame, le WINDOW_UPDATE et l'ordre des headers HTTP/2
forment un fingerprint (le « Akamai HTTP/2 fingerprint »). reqwest 0.12 + hyper 1.x
ont un ordre de headers déterministe qu'on peut rendre aléatoire via le
`BrowserProfile`.

### 3.6 Proxy + IP reputation

Hors de notre contrôle côté serveur (datacenter 192.168.1.x). Le FlareSolverr résout
ce point en sortant par son pool résidentiel. **C'est pour ça qu'on escalade vers
FlareSolverr sur Leboncoin déjà**.

---

## 4. Recommandations par site

### Stack Overflow ✅ (ne pas toucher)
CDP direct = 18K chars. Aucun gap.

### Amazon.fr
- **Home `/` et pages recherche** : actuellement 0 char / 202. Cause probable : pas de
  cookie `session-id` et TLS fingerprint non-Chrome.
- **Pages produit** : 200 + 404-Amazon pour les mauvais ASIN, mais **un vrai produit
  (par ex. un bestsellers item) marche**. Donc le HTTP direct suffit pour les pages
  produit populaires, pas pour la nav.
- **Action** : escalader vers **CDP stealth** dès que la réponse HTTP < 200 chars
  **ou** contient `<title>Page introuvable</title>` (404 Amazon ≠ anti-bot, mais page
  produit « non trouvée » doit escalader). Et **escalader vers FlareSolverr** si le
  CDP renvoie encore 0 char après 5s.

### Leboncoin.fr
- **Home** : 8761 chars en CDP — partiel (DataDome bloque probablement le rendu
  complet d'iframe pub, mais le HTML principal passe).
- **Pages annonce `/ad/.../{id}`** : risque de bloc DataDome dur.
- **Page recherche `/recherche?...`** : JSON embarqué `__NEXT_DATA__` — notre extract
  actuel prend le HTML rendu, pas le JSON.
- **Actions** :
  1. **Cookie persistence** : sauver le `dd` cookie de Leboncoin (CDP ou FS) et le
     renvoyer sur la requête HTTP suivante.
  2. **Détection 0-char** : si le HTML rendu par CDP contient `datadome.co` ou
     `dd-captcha`, escalader FlareSolverr (qui a un pool FR).
  3. **Extraction `__NEXT_DATA__`** : bonus, peut être ajouté dans `extract/content.rs`
     plus tard.

---

## 5. Top-3 améliorations à implémenter

Hiérarchie par **ratio impact/effort** :

### #1 — Cookie persistence entre fetchers (impact fort, effort faible)
- Ajouter un `CookieJar` partagé dans `HttpFetcher` et dans `CdpFetcher` (via un
  trait object).
- À chaque réponse HTTP/CDP, extraire les `Set-Cookie`, les merger dans le jar.
- À chaque requête sortante, ajouter le header `Cookie:` correspondant au host.
- **Effet** : Leboncoin DataDome `dd` cookie, Amazon `session-id` / `x-amz-rid`,
  Cloudflare `cf_clearance`. Sans ça, chaque requête repart à zéro.

### #2 — Escalade auto vers CDP quand la page est « vide » (impact fort, effort faible)
- Dans `ladder::http_is_challenge`, ajouter le cas :
  - `status 200 && html.len() < 500` (page anti-bot « spinner » sans contenu)
  - `status 200 && html.contains("Page introuvable")` (Amazon 404)
- Et symétriquement dans `ladder::try_cdp` : si le HTML CDP < 500 chars OU contient
  `datadome.co` / `dd-captcha`, **escalader FlareSolverr** directement (au lieu de
  renvoyer le résultat CDP vide).

### #3 — Realistic timing + mouse micro-movements (impact moyen, effort moyen)
- Dans `cdp.rs::run_fetch`, après le `goto`, ajouter un petit script JS qui :
  - Déplace la souris en 3-5 points via `Input.dispatchMouseEvent` (CDP).
  - Scroll incrémental avec pauses.
  - Attend que `document.readyState === 'complete'` + 500 ms.
- Etendre `BrowserProfile` avec `behavior = { mouse: bool, scroll: bool }` pour
  désactiver sur les sites rapides.

### Honneur — TLS fingerprint (effort très fort, pas implémenté maintenant)
- Nécessite soit un fork de `boring` (projet `boringtls`), soit un binding
  `curl-impersonate`. Compilé en C via `cc` crate. Décision : reporter après les
  3 quick wins.

---

## 6. Plan d'implémentation

```
1. Cookie jar
   - crates/antibot/src/cookie_jar.rs   (nouveau, minimal : domain-keyed HashMap<name,value>)
   - crates/fetch/src/http.rs          (utiliser le jar sur send, écrire sur response)
   - crates/fetch/src/cdp.rs           (utiliser Network.setExtraHTTPHeaders ou Cookie header,
                                        écrire sur les cookies CDP via Network.getCookies)
   - crates/fetch/src/ladder.rs        (partager le jar entre HTTP et CDP)

2. Empty-page detection
   - crates/fetch/src/ladder.rs        (modifier http_is_challenge, ajouter cdp_is_empty)
   - crates/antibot/src/challenge_detect.rs  (ajouter detect_amazon_404, detect_datadome_block)

3. Realistic timing + mouse
   - crates/fetch/src/cdp.rs           (ajouter humanise_pre_extract après le goto)
   - Optionnel: crates/antibot/src/cdp_stealth.rs (webdriver = false au lieu de undefined,
                                                    seed canvas par session)
```

### Critères de succès

| Site               | Baseline | Objectif             |
|--------------------|----------|----------------------|
| Amazon home `/`    | 0 chars  | > 5000 chars         |
| Amazon `/gp/bestsellers/` | 42459 | ≥ 42459 (régression) |
| Amazon search `s?k=...` | 0 | > 2000 chars |
| Leboncoin home     | 8761     | > 12000 chars        |
| Leboncoin `/ad/...`| (n/a)    | > 3000 chars         |
| StackOverflow      | 18789    | ≥ 18789              |

Tests : ajouter 5-10 tests unitaires pour les nouveaux helpers de détection. Tous
les tests existants doivent rester verts.
