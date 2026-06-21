# Bench sites durs (post v7) — 5 anti-bot critiques

**Date** : 2026-06-21 10:35 Paris (CEST)
**Image** : crw-shield-crw-shield:latest (1.28 GB, v7 = QW#1-#4 quick wins)
**Container** : up sur :3002, health OK
**Endpoint HITL** : POST /v2/scrape/hitl — testé OK (file d'attente créée)

## Résultat

| Site | Statut | MD | Temps | Diagnostic |
|---|---|---|---|---|
| amazon.fr | FAIL | 0 | 17.0s | HTTP 502 (FlareSolverr timeout) |
| leboncoin.fr | FAIL | 0 | 14.1s | HTTP 502 (DataDome) |
| etsy.com | FAIL | 0 | 3.5s | HTTP 502 (DataDome captcha) |
| stackoverflow.com/q/1 | FAIL | 0 | 5.8s | HTTP 502 (Cloudflare IUAM) |
| linkedin.com | FAIL | 0 | 5.4s | HTTP 502 (LinkedIn bot) |

**Score : 0/5** sur les sites vraiment durs. Identique à la baseline.

## Conclusion honnête

Les 4 quick wins implémentés (cookie jar partagé, empty-page detection, humanisation, endpoint HITL) **n'ont pas fait reculer le plafond anti-bot**. Les 5 sites résistent toujours pour la même raison : **IP datacenter + DataDome/Cloudflare IUAM scoring** — c'est pas une question de stack, c'est une question d'IP.

L'endpoint HITL est maintenant fonctionnel. Pour Etsy/Leboncoin/etc, le workflow est :
1. `POST /v2/scrape` → FAIL
2. `POST /v2/scrape/hitl {"url": "https://etsy.com/..."}` → enregistre dans queue
3. Humain ouvre l'URL dans Playwright Desktop, résout le captcha, écrit les cookies dans `/tmp/hitl_queue.json`
4. `POST /v2/scrape` (avec cookies du HITL) → OK

Mais ce workflow n'a pas été testé bout-en-bout (pas de HITL actif dans cet env). Le score **reste à 0/5 sur les sites durs** sans HITL actif.

## Pour vraiment passer ces 5 sites

- **HITL actif** (humain à la mano) : gain partiel, dépend de l'humain
- **Proxies résidentiels** ($50-200/mois) : gain x10, seule vraie solution
- **Accepter le plafond** : crw-shield = "clone Firecrawl Rust 100% local, raw content, pas de quota" — déjà la valeur ajoutée
