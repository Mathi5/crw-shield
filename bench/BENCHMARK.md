# crw-shield benchmark log

Tracked at each step. Goal: panel test 15 sites, measure OK count + quality.

## Baseline (avant plan, commit ed68c50)
- 4-5/15 OK (Wikipedia Cloudflare, GitHub, BBC, Le Monde, Leboncoin)
- 5/15 bloqués anti-bot réel (Etsy, Leboncoin bloqué, SO, old.reddit, Amazon bestsellers)
- 2/15 faux positifs (Wikipedia Rust antibot à tort, HackerNews)
- 4/15 timeouts HTTP (reddit.com, rust-lang.org, Amazon product, Twitter)

## Étapes
2026-06-20 15:31 UTC

### Étape A1+A2 — Debug live terminé (2026-06-20 15:32 UTC)

**Panel 6 sites via /v2/scrape :**
| Site | Résultat | Diagnostic |
|---|---|---|
| rust-lang.org | FETCH_ERROR 502 | 301 mal suivi par wreq |
| reddit.com | 200 + JsOnly+ladder=Cdp | CDP tenté, échec (no chromium), FlareSolverr pas wiré ensuite |
| amazon.fr/dp/B0BSHF7WHW | FETCH_ERROR 502 | Boucle redirect Amazon |
| twitter.com | FETCH_ERROR 502 | 301 vers x.com pas suivi |
| Wikipedia Rust | md=36655 GenericAccessDenied | **Faux positif** : 36k extrait mais classifié antibot |
| news.ycombinator.com | md=11077 CleanSuccess | **OK !** Faux positif du benchmark initial résolu par fixes successifs |

**Conclusion diagnostic :**
1. HackerNews = résolu tout seul (CleanSuccess)
2. 3 FETCH_ERROR = redirect 301 mal géré par wreq dans `crates/fetch/src/http.rs`
3. Reddit = ladder CDP fail → FlareSolverr pas appelé → erreur silencieuse
4. Wikipedia Rust = token antibot trop agressif (à durcir : 2 token matches min OU 1 token ≥6 chars)


### Étape A3 — Patches appliqués + B1+B2 préparés (commit 33116c9)

**Code modifié/ajouté :**
- `crates/fetch/src/tls_profile.rs` : redirect fix + Firefox123 enum
- `crates/fetch/src/ladder.rs` : L1/L2 logging + `fetch_with_rotation()` wrapper
- `crates/antibot/src/situation.rs` : token gate 2xx (2+ hits OU 1 hit ≥14 chars)
- `crates/antibot/src/lib.rs` : exports Firefox/rotation/block_detection
- `crates/antibot/src/firefox_profiles.rs` : 3 profils Firefox (260 LOC, 6 tests)
- `crates/antibot/src/block_detection.rs` : port cortex-bridge MIT (290 LOC, 9 tests)
- `crates/antibot/src/rotation.rs` : L0-L3 ladder (200 LOC, 6 tests)

### Étape A4 — Validation panel 6 sites post-patch

| Site | ed68c50 (baseline) | 33116c9 (post-patch) | Δ |
|---|---|---|---|
| rust-lang.org | FETCH_ERROR 502 | **md=3368 CleanSuccess** | ✅ RÉSOLU |
| reddit.com | 200 JsOnly+ladder=Cdp | FETCH_ERROR 502 | ⚠️ logs propres mais tjrs bloqué |
| amazon.fr/dp/... | FETCH_ERROR 502 | FETCH_ERROR 502 | ❌ pas changé (DataDome) |
| twitter.com | FETCH_ERROR 502 | FETCH_ERROR 502 | ❌ pas changé |
| Wikipedia Rust | md=36655 GenericAccessDenied | FETCH_ERROR 502 | ⚠️ token gate OK mais escalade CDP fail |
| HackerNews | md=11077 CleanSuccess | md=11072 CleanSuccess | ✅ stable |

**Verdict A4 :**
- 1 vrai succès nouveau (rust-lang.org grâce au redirect fix)
- 1 stable (HackerNews)
- 4 bloqués structurellement (CDP/FlareSolverr absent du container prod)
- Les logs sont maintenant explicites (`WARN Ladder exhausted: CDP failed and FlareSolverr unavailable`)
- Le faux positif Wikipedia Rust ne se manifeste plus (token gate fonctionne)

**Score global : 2/6 fonctionnel** (vs 1/6 baseline), mais surtout les logs diagnostiques sont exploitables pour la suite.

**Conclusion :** Les patches A3 ont livré **+1 site fonctionnel net + logs propres + faux positif antibot éliminé**. Les autres sont structurellement bloqués (anti-bot réel sans CDP/FS dans la prod). B1+B2 code est en place mais pas wiré dans le handler HTTP — `fetch_with_rotation()` existe mais `handlers.rs::scrape()` n'appelle toujours que `ladder.fetch()`.

### Étape B2.2 — Wire `fetch_with_rotation()` dans handlers.rs + activation FlareSolverr (commit f6c8486 + docker-compose patch unstaged)

**Modifications :**
- `crates/server/src/state.rs` : ajout `host_counters: HostCounters` dans `AppState`
- `crates/server/src/handlers.rs` : `state.ladder.fetch_with_rotation(&req, &state.host_counters)` au lieu de `state.ladder.fetch(&req)`
- `docker-compose.yml` : décommenté `FLARESOLVERR_URL=http://192.168.1.101:8666`

**A5 — Panel 6 sites post-B2.2 + FlareSolverr actif (2026-06-20 17:42 UTC, image f6c8486)**

| Site | ed68c50 | 33116c9 | **f6c8486 + FS** | Δ depuis baseline |
|---|---|---|---|---|
| rust-lang.org | FETCH_ERROR | md=3368 OK | **md=3379 OK** | ✅✅ |
| reddit.com | JsOnly+Cdp | FETCH_ERROR | **md=35504 OK** | ✅✅ (FS débloque) |
| amazon.fr/dp/... | FETCH_ERROR | FETCH_ERROR | **FETCH_ERROR** | ❌ DataDome+CDP bloque |
| twitter.com | FETCH_ERROR | FETCH_ERROR | **md=803 OK** | ✅✅ (FS débloque) |
| Wikipedia Rust | GenericAccessDenied | FETCH_ERROR | **md=29444 OK** | ✅✅ |
| HackerNews | md=11077 OK | md=11072 OK | **md=11030 OK** | ✅✅ stable |

**Verdict A5 :**
- **5/6 ✅ (83%)** vs 2/6 (33%) à 33116c9, vs 1/6 baseline
- Seul **Amazon DataDome** reste bloqué — son anti-bot est au-dessus de ce que FlareSolverr peut passer sans navigateur persistant
- Le token gate antibot fonctionne : Wikipedia Rust ne déclenche plus de faux positif
- Logs très propres : `INFO scrape request` + `WARN HTTP response triggers escalation` + `INFO rotation` selon le cas

**Conclusion finale :** les patches A3 + B2.2 + FlareSolverr ont permis de débloquer reddit.com, twitter.com et wikipedia.org en plus du rust-lang.org déjà résolu. 5/6 sites fonctionnels. Le seul résidu est Amazon, dont l'anti-bot (DataDome) est conçu spécifiquement pour bloquer les proxies datacenter comme FlareSolverr. Solutions restantes : navigateur persistant (Playwright/CDP headful) ou proxies résidentiels (Volet C abandonné).

### Recommandation prochaine étape
- Option 1 (rapide, **FAIT**) : wire `fetch_with_rotation()` dans handlers.rs + activer FlareSolverr → ✅ 5/6
- Option 2 (lourd) : ajouter chromium au container prod + Playwright/CDP → +1-2 sites supplémentaires (Amazon, Leboncoin bloqué) mais +200MB image, +5-10 min build
- Option 3 (rebranding) : ship ce qu'on a (5/15 estimé), doc, passer à d'autres features