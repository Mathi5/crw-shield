# Anti-bot landscape 2026 — diagnostic post Phase D

**Date** : 2026-06-21
**Contexte** : serveur derrière box FAI perso (IP résidentielle, pas datacenter)

## Verdict : FlareSolverr est une mauvaise idée en 2026

### Preuve directe (réalisée pendant la session)

Test direct FlareSolverr v3.5.0 sur Etsy (POST 192.168.1.101:8666/v1) :
```
status=ok message="Challenge not detected!"
url=https://www.etsy.com/ http=200
html_len=1488 cookies=2
```

Le HTML est un **DataDome Device Check interstitial** (`ct.captcha-delivery.com`) renvoyé tel quel. FlareSolverr dit "no challenge detected" — il ne reconnaît même pas DataDome. **C'est un faux succès** : un user voyait `success=true` mais le contenu est vide.

### Sources externes (mai 2026)

**Ian L. Paterson — Anti-detect browser benchmark 2026** (31 cibles Cloudflare, 7 outils, IP résidentielle) :
- **nodriver** = 28/31 OK, **0 blocked** ← winner
- CloakBrowser = 26/31 (Chromium 49 patches C++)
- curl_cffi = 26/31 (HTTP-only, 6.4MB wheel)
- Patchright = 25/31
- Camoufox = 25/31
- **FlareSolverr** = weak tier, JS-layer patches only

**Key insight** : *"Automation-protocol fingerprinting (how the browser is driven) matters more than JS/TLS fingerprint patches."*

**BotCloud — FlareSolverr alternatives 2026** :
> "The FlareSolverr tier is honestly weak in 2026. It uses upstream Chromium without engine-level fingerprint modifications. Patches are JavaScript-layer, which Cloudflare's iframe-side checks have learned to read. Still passes low-tier sites; doesn't pass current Bot Fight Mode or Pro Cloudflare WAF rulesets."

### Changlogs FlareSolverr v3.4.6 → v3.5.0
- v3.5.0 : "Resolve turnstile captcha" (Cloudflare only)
- v3.4.6 : "Add disable image, css, fonts option with CDP"
- v3.4.5 : "Revert to Python 3.13"
- **DataDome n'est JAMAIS mentionné.**

## Architecture anti-bot : deux familles opposées

### Active solver (FlareSolverr, 2Captcha, CapSolver)
- Lance un solveur externe à chaque challenge
- 5-30s de latence par appel, $0.001-0.003/token
- Convaincu de "résoudre" le captcha à la place de l'humain
- **Limite** : ne sait gérer que ce qu'il a été programmé pour gérer (Cloudflare oui, DataDome non)

### Passive auto-pass (nodriver, CloakBrowser, Camoufox)
- Le browser fingerprint est tellement bon que le challenge ne se déclenche même pas
- 0 latence, coût mensuel fixe ($0 self-hosted, $99-299/mois managed)
- **Limite** : maintenance du patch pour suivre les évolutions des anti-bots

**crw-shield = hybride bancal** : il a chromiumoxide (CDP direct, bien) + stealth JS (patch layer, comme FlareSolverr) + FlareSolverr (active solver, weak tier) + cookie jar + JA3. Il n'est ni vraiment passif (Chromium vanilla sans patches engine-level) ni vraiment actif (FlareSolverr qui marche pas sur DataDome).

## Plan révisé

### Option A — Remplacer le browser engine par CloakBrowser ou Camoufox
- **CloakBrowser** = Chromium 145 fork avec 49 patches C++ (drop-in Playwright API). Mais on est en Rust, pas Playwright.
- **Camoufox** = Firefox 135 fork avec C-level fingerprint spoofing (MPL-2.0). Utilisable en Rust via `camoufox` binary.
- **Coût** : 0€ (OSS), 1-2 jours de dev pour intégrer
- **Gain attendu** : 26/31 → potentiellement 25/31 (Camoufox = 25, CloakBrowser = 26 sur le benchmark Paterson)

### Option B — Remplacer FlareSolverr par nodriver côté Python (subprocess)
- Lancer nodriver via subprocess depuis crw-shield
- nodriver gère Chrome 148 direct via CDP, sans Playwright shim
- **Coût** : 0€ (Python+AGPL), 2-3 jours de dev (subprocess + cookie passing)
- **Gain attendu** : 28/31 sur le benchmark Paterson = meilleur que toutes les alternatives OSS

### Option C — Passer à curl_cffi-like (TLS-only, pas de JS)
- Abandonner le browser pour les sites simples
- Utiliser `wreq` (déjà en place) avec des profils TLS encore plus complets
- Garder chromium que pour les sites JS-only
- **Coût** : 0€ (on a déjà wreq), 1 jour de dev
- **Gain attendu** : même score qu'aujourd'hui, mais latence divisée par 5 sur 80% des sites

### Option D — Accepter le plafond (pivoter)
- crw-shield = clone Firecrawl Rust 100% local
- 16/20 sur 20 sites = honnête et utile
- Pas la peine de chase cortex-bridge
- **Coût** : 0, 0 jour de dev
- **Gain** : zéro, mais paix mentale

### Recommandation : Option C + Option D combinées
- **Court terme (1 jour)** : Option C — abandonner FlareSolverr, faire confiance à wreq pour les sites simples, garder chromium pour JS-only. Ça suffit pour 80% des sites.
- **Moyen terme (1 semaine)** : Option D — pivoter le positionnement, documenter crw-shield comme "clone Firecrawl Rust sans solveur commercial"
- **Si tu veux vraiment chasser cortex-bridge** : Option A (Camoufox) en plus.

## Action immédiate proposée

1. **Désactiver FlareSolverr** dans docker-compose.yml (le `FLARESOLVERR_URL` env var)
2. Re-bench 20 sites → on verra si le score baisse (peut-être que FlareSolverr aidait sur certains sites) ou reste stable
3. Si baisse > 5% sur des sites précis, le ré-activer opt-in par site via le hint `force_flaresolverr` dans ScrapeRequest
4. Documenter le choix dans le README

**Pas besoin d'attendre** : c'est 1 ligne de changement + 1 re-bench.
