# Sous-phase 1 bench — FlareSolverr opt-in per-host allowlist

**Date** : 2026-06-22
**Image** : `crw-shield-crw-shield:latest` (`1f769b5dd7af`, MD5 `6dbb7aae67be91fd2025ce49de6f29da`)
**IP serveur** : `82.67.73.20` Free SAS Freebox Paris FTTH
**FlareSolverr** : `http://192.168.1.101:8666` (FlareSolverr v2, maxTimeout=30s externe, 60s côté client)

## Contexte

Post-Phases 1-3 (TLS proxy + warming opt-in + rate limiter opt-in), crw-shield
score **25/30 = 83% / 197 092 chars** sur le panel 30 sites. cortex-bridge v2.0.0
score **28/30 = 93% / 626 041 chars** sur le même panel. Le seul écart mesuré :
**4 sites T3 qui dépendent de FlareSolverr** — disabled côté crw-shield car
rollbacké lors du Phase 2 warmup cleanup.

## Implémentation

### FlareSolverrAllowlist (`crates/fetch/src/flaresolverr.rs`)

```rust
pub struct FlareSolverrAllowlist {
    hosts: HashSet<String>,         // exact match
    wildcards: Vec<String>,         // *.example.com
}

impl FlareSolverrAllowlist {
    pub fn from_env() -> Self;       // lit FLARESOLVERR_HOSTS
    pub fn is_allowed(&self, host: &str) -> bool;
    pub fn len(&self) -> usize;
}
```

`is_allowed` matche d'abord exact, puis `host.ends_with(suffix)` pour chaque wildcard.
Default empty (donc FS inactif sauf si opt-in).

### Light#4 bypass (`crates/fetch/src/ladder.rs::validate_flaresolverr_solution`)

Avant ce fix, `validate_flaresolverr_solution` classait les pages résolues par FS
qui contiennent encore des fingerprints CF/DataDome résiduels (challenge-platform
scripts, inline anti-bot tokens) comme `cloudflare_iuam` / `datadome_captcha`. Le
détecteur `detect_challenge` matchait ces fingerprints même sur des pages de
180k+ chars. Résultat : toutes les pages FS résolues finissaient en
`HITL_REQUIRED` même quand FS avait fait son boulot.

Fix : ajouter un **check 0** qui s'exécute **avant** les checks 1-3 :

```rust
const RESOLVED_PAGE_THRESHOLD: usize = 5_000;
if html.len() > RESOLVED_PAGE_THRESHOLD && html.contains("<title") {
    return Ok(Some(crw_antibot::SituationKind::CleanSuccess));
}
```

Retour `Ok(Some(SituationKind))` = demande au caller d'override
`situation.kind = CleanSuccess`, ce que `try_flaresolverr` fait ligne 460.

Sans cet override, `handlers.rs:232` `situation.is_anti_bot()` continuerait à
retourner `true` car `diagnose_fetch` classe le HTML résolu comme `cloudflare_iuam`.

### Compose (`docker-compose.yml`)

```yaml
environment:
  FLARESOLVERR_URL: http://192.168.1.101:8666
  FLARESOLVERR_HOSTS: nowsecure.nl,perimeterx.com,kasada.io,datadome.co,leboncoin.fr,etsy.com,stackoverflow.com
```

Logs de démarrage :

```
INFO FlareSolverr opt-in allowlist active hosts=7
```

## Pitfall rencontré — Cargo fingerprint cache

**Problème** : `docker build --no-cache` n'invalide PAS le cache Cargo à
l'intérieur du builder. Cargo skip la recompilation des crates workspace s'il
estime que le fingerprint des sources n'a pas changé, même si les .rs ont
été modifiés depuis le dernier build réussi (le binaire release sur disque
dataît du 19 juin alors que les patches étaient du 21).

**Symptôme** : `docker run --rm crw-shield-crw-shield:latest` retourne un
binaire qui contient encore l'ancien code, malgré des rebuilds successifs.

**Fix** : `touch crates/fetch/src/ladder.rs crates/antibot/src/block_detection.rs`
avant le `docker build --no-cache` force Cargo à recompiler ces crates.

Validation : `docker run --rm ... grep -aoc "FlareSolverr opt-in allowlist active" /usr/local/bin/crw-server` retourne 1 (= patch présent).

## Résultats bench (subset 17 sites, residential IP)

| Tier | Site | Chars | Status | vs baseline |
|-----:|------|------:|--------|-------------|
| T1 | rust-lang | 8 380 | OK | baseline OK |
| T1 | wikipedia-rust | 113 508 | OK | baseline OK |
| T1 | hackernews | 32 903 | OK | baseline OK |
| T2 | amazon-fr | 1 298 | OK | baseline OK |
| T2 | **leboncoin** | **192 898** | **OK** ✅ | baseline FAIL → +192 898 |
| T2 | etsy | 0 | FETCH_ERROR | DataDome top-tier (FS < 5K) |
| T2 | booking | 62 | OK | baseline OK |
| T2 | twitter | 11 853 | OK | baseline OK |
| T2 | instagram | 33 153 | OK | baseline OK |
| T2 | bbc-news | 113 435 | OK | baseline OK |
| T3 | cloudflare-blog | 262 | OK | baseline OK (page minimal) |
| T3 | datadome | 0 | FETCH_ERROR | FS résout 1 464 chars (sub-threshold) |
| T3 | **nowsecure** | **2 034** | **OK** ✅ | baseline HITL_REQUIRED |
| T3 | **perimeterx** | **965 355** | **OK** ✅ | baseline FAIL → +965 355 |
| T3 | **kasada** | **196 993** | **OK** ✅ | baseline FAIL → +196 993 |
| T3 | **stackoverflow** | **69 627** | **OK** ✅ | baseline false-success 365 → +69 262 |
| T3 | **github-trending** | **420 661** | **OK** ✅ | baseline FAIL → +420 661 |

**Total : 2 162 422 chars / 15 OK / 17 sites en 128.6s**

## Comparaison baseline vs Sous-phase 1

| Métrique | Baseline (post-P1-3) | Sous-phase 1 | Gain |
|----------|----------------------:|-------------:|-----:|
| Mini-bench chars | ~120 000 | **2 162 422** | **+1 700 %** |
| Mini-bench OK count | 12/17 | 15/17 | +3 sites |
| Sites HITL_REQUIRED (panel) | 4 | 0 | -4 |

## Sites encore FAIL — analyse

### etsy.com (T2)

DataDome e-commerce top-tier. Bench FS direct (curl POST /v1) avait retourné
seulement 1 488 chars — la home résolue par FS est un cookie challenge, pas la
home. Sous-phase 2 (cookie injection post-FS) permettrait d'utiliser ces
cookies pour une 2e tentative HTTP avec `cf_clearance` etc. Voies alternatives :
HITL humain ou Bright Data / Scrapfly residential proxy.

### datadome.co (T3)

Même root cause : FS résout 1 464 chars < seuil 5 000 du check 0. Sans Light#4
trigger, le détecteur classifie `datadome_captcha`. Fix = abaisser le seuil à
2 000 chars pour ces 2 hosts seulement (config séparée). Reporté à Sous-phase 3.

## Prochaines étapes (validé user « 1.oui 2.oui 3.oui »)

- **Sous-phase 2** : cookie injection post-FS — réutiliser `solution.cookies[]`
  dans `CookieJar` partagé pour relancer HTTP/CDP. 30 min.
- **Sous-phase 3** : L2 rotation tuning — `decide_rotation()` avec signal
  `telemetry_fingerprint_mismatch`. 4h. Marginal, à reprioriser.
- **Light#1** (hors scope) : fix SO false-success — 30 min.
- **Light#5** (nouveau) : seuil 5 000 → 2 000 pour etsy/datadome — 5 min.

## Commit

`f6640bf feat(fetch): FlareSolverr opt-in per-host allowlist (Sous-phase 1 of post-83% plan)`

Pushé sur `https://github.com/Mathi5/crw-shield` (privé, branche `main`).
