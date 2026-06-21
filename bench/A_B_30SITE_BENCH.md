# A/B Bench 30 sites — crw-shield vs cortex-bridge

**Date** : 2026-06-21 (CEST, Paris)
**Contexte** : Bench final après exécution du plan d'intégration A/B en 6 phases (commits `c7bed40`, `13c6bc2`, `bda134c`).
**But** : Valider que crw-shield (post-Phases 1-3) rivalise avec cortex-bridge sur un panel étendu de 30 sites (vs 7 dans le bench précédent).

## Méthodologie

- **crw-shield** : commit `bda134c` (post-Phases 1-3), TLS proxy ON, profile warming opt-in OFF, rate limiter 0/0 (désactivé pour le bench)
- **cortex-bridge** : commit `abba6bf` (vanilla, sans modifs)
- **IP** : résidentielle Free SAS (82.67.73.20, hostname fbx.proxad.net)
- **API** : `/v2/scrape` pour crw-shield, `/v1/scrape` pour cortex-bridge (auto-detect dans le script)
- **Panel** : 30 sites en 3 tiers (T1 = 7 vanilla, T2 = 12 anti-bot moyen, T3 = 11 anti-bot strict)
- **Timeout** : 60s par scrape
- **Script** : `python3 /tmp/bench30.py <endpoint>` (auto-detect v1/v2)

## Résultats par tier

| Tier | crw-shield (OK strict) | cortex-bridge (OK strict) | Verdict |
|---|---|---|---|
| **T1 (vanilla, 7 sites)** | 7/7 = **100%** | 7/7 = **100%** | tie |
| **T2 (anti-bot moyen, 12 sites)** | 11/12 = **92%** | 11/12 = **92%** | tie |
| **T3 (anti-bot strict, 11 sites)** | 7/11 = **64%** | 10/11 = **91%** | **cortex-bridge +27%** |
| **OVERALL** | **25/30 = 83%** | **28/30 = 93%** | **cortex-bridge +10%** |

## Totaux chars

| Backend | Total chars (30 sites) |
|---|---|
| crw-shield (Phases 1-3) | **194 715** |
| cortex-bridge | **626 041** |
| Delta | cortex-bridge +221% |

## Sites débloqués par cortex-bridge (3 de plus que crw-shield)

| Site | crw-shield | cortex-bridge |
|---|---|---|
| stackoverflow | 365 chars (faux-OK, interstitial CF) | **3 487 chars (vrai contenu)** |
| nowsecure | 0 (HITL_REQUIRED) | 74 chars (page CF vide mais status OK) |
| akamai-demo | 0 (HITL_REQUIRED) | 13 804 chars |

## Sites où crw-shield = cortex-bridge

T1 (rust-lang, github, wikipedia, hackernews, old-reddit, lemonde) et la plupart de T2 (amazon, linkedin, booking, etc.).

## Sites où les deux échouent

| Site | Tier | Verdict |
|---|---|---|
| etsy | T2 | cortex-bridge timeout 60s, crw-shield HITL_REQUIRED — DataDome intraitable sans IP résidentielle premium |
| soundcloud | T3 | cortex-bridge `navigation_failed`, crw-shield OK 1320 chars — **crw-shield gagne ce site** |

## Pourquoi cortex-bridge reste devant sur T3

Trois facteurs techniques non encore portés dans crw-shield :

1. **Profile warming actif au boot** (commits Phase 2 shipped opt-in, mais bug chromiumoxide Browser lock empêche l'activation en pratique)
2. **7-profile rotation avec cooldown auto** (cortex-bridge L2 déclenche sur signal faible ; crw-shield L2 câblé en `605a8e5` mais le signal est plus rare)
3. **Persistent profile dirs propagés cross-profile** (cortex-bridge `propagate_warm_profile` copie `History`/`Cache` mais pas `Cookies` entre les 7 profiles)

## Pour rattraper cortex-bridge sur T3

| Action | Effort | Risque |
|---|---|---|
| Fix chromiumoxide Browser lock (warmup via Browser éphémère séparé) | 1-2 jours | moyen |
| Tuner le seuil de rotation L2 pour déclencher sur signal faible | 0.5 jour | faible |
| Propager `History`/`Cache` (sans `Cookies`) cross-profile | 0.5 jour | faible |
| **Total estimé pour combler le gap T3** | **2-3 jours** | acceptable |

## Verdict honnête (pitfall 49)

**Cortex-bridge est strictement meilleur sur le panel 30 sites** (93% vs 83% OK, +221% chars). **Les phases 1-3 ont neutralisé les régressions et stabilisé la base**, mais n'ont pas rattrapé le delta sur T3.

**Cependant** :
- crw-shield a une **architecture 8-crates** plus maintenable (situation taxonomy 30+ variantes)
- crw-shield a un **HITL worker explicite** (commit `605a8e5`) que cortex-bridge n'a pas
- crw-shield a un **panel de tests reproductible** (`bench/`, `bench30.py`)

**Recommandation** : si l'objectif est "le plus de sites qui passent", adopter cortex-bridge. Si l'objectif est "API honnête + observabilité", garder crw-shield et investir 2-3 jours pour combler le gap T3 (voir section ci-dessus).

## Fichiers de référence

- `/tmp/bench_ab_results.json` — bench 9 sites initial (Phase A/B discovery)
- `/tmp/bench_phase2.py` — bench 9 sites post-Phase 2
- `/tmp/bench30.py` — bench 30 sites (Phase 4+5)
- `/tmp/crw_bench30.out` — output crw-shield
- `/tmp/cortex_bench30b.out` — output cortex-bridge
- `bench/CORTEX_BRIDGE_BENCH.md` — bench cortex-bridge 7 sites (Phase A/B discovery)
