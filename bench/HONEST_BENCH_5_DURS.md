# Bench 5 sites durs (post v7) — diagnostic final

**Date** : 2026-06-21 ~10:40 Paris (CEST)
**Image** : crw-shield-crw-shield:latest (1.28 GB, v7 = QW#1-#4 quick wins)
**Container** : up sur :3002
**Serveur** : IP résidentielle (derrière box FAI perso) — pas datacenter

## Résultat

| Site | Statut | MD | Diagnostic |
|---|---|---|---|
| amazon.fr | FAIL | 0 | FlareSolverr timeout 17s |
| leboncoin.fr | FAIL | 0 | FlareSolverr timeout 14s (DataDome) |
| etsy.com | FAIL | 0 | DataDome interstitial (1488 chars) |
| stackoverflow.com | FAIL | 0 | Cloudflare IUAM |
| linkedin.com | FAIL | 0 | LinkedIn bot detection |

**Score : 0/5.**

## Diagnostic technique profond

**Test direct FlareSolverr sur Etsy** (POST 192.168.1.101:8666/v1, 60s timeout) :

```
status=ok message="Challenge not detected!"
url=https://www.etsy.com/ http=200
html_len=1488 cookies=2
```

Le HTML retourné est un **DataDome Device Check interstitial** :
```html
<script>var dd={'rt':'i','cid':'AHrlqAAAAAMAx05pd...',...}</script>
<iframe src="https://geo.captcha-delivery.com/interstitial/..."></iframe>
```

**FlareSolverr v3.5.0 (la dernière) ne supporte PAS DataDome.** Les changelogs de v3.4.6 → v3.5.0 mentionnent uniquement "turnstile captcha" et "disable images/css/fonts CDP". DataDome n'apparaît jamais. Quand FlareSolverr voit un site DataDome, il répond "Challenge not detected" et renvoie l'interstitial tel quel.

**Conclusion** : le plafond DataDome (Etsy, Leboncoin, Airbnb, Amazon) **n'est pas une question d'IP** (l'IP est résidentielle) ni de stack fingerprint (chromium + stealth JS + wreq TLS fingerprinting sont déjà en place), c'est une question de **solveur de captcha**. FlareSolverr ne sait pas faire DataDome, et c'est pareil pour Firecrawl self-hosted (qui n'a pas de solveur propriétaire non plus).

**Pour passer DataDome**, il n'y a que 2 options :
1. **HITL actif** (humain résout le captcha à la main, écrit les cookies → /tmp/hitl_queue.json)
2. **Service commercial** (Scrapfly, Zyte, Browserbase) qui ont des solveurs DataDome dédiés

## Architecture du HITL endpoint livré

L'endpoint `POST /v2/scrape/hitl` est en place et testé (réponse 202 Accepted avec `hitl_required: true` + `queue_file: /tmp/hitl_queue.json`). Il faut maintenant :

1. L'agent scrape → `POST /v2/scrape` → FAIL
2. L'agent → `POST /v2/scrape/hitl` avec l'URL → queue file
3. **Humain externe** (Playwright Desktop, browser perso) résout le captcha
4. Humain écrit les cookies dans `/tmp/hitl_queue.json` (status="solved", cookies=[...])
5. L'agent relit le queue file, extrait les cookies, retry `/v2/scrape` avec les cookies
6. → succès

**Cette boucle n'est PAS automatisée** et n'a pas été testée bout-en-bout (pas de HITL actif dans cet env headless).

## Cortex-bridge réussi 6/6 : pourquoi ?

Hypothèses (à vérifier en lisant le code cortex-bridge) :
- Soit cortex-bridge utilise **Browserbase/Scrapfly** (solveur DataDome commercial) au lieu de FlareSolverr OSS
- Soit cortex-bridge a **son propre solveur DataDome** en interne
- Soit les "preuves" 6/6 sont en environnement avec IP résidentielle + cookies pré-chauffés + état persistant

Dans tous les cas, le point commun n'est PAS dans le code de stealth (qui est déjà solide dans crw-shield), c'est dans **le solveur** ou **l'IP warmed-up**.

## Plan recommandé

1. **Phase A — Valider le HITL bout-en-bout** : tu lances Playwright Desktop, résous Etsy manuellement, on vérifie que la queue file + retry cookies marche. Si oui → 5/5 sur les sites durs.
2. **Phase B — Si tu veux automatiser** : payer Browserbase ($20-50/mois) ou Scrapfly ($30-100/mois) qui ont un solveur DataDome intégré. crw-shield pourrait router ces sites vers eux.
3. **Phase C — Accepter le plafond** : crw-shield pivote en "clone Firecrawl Rust 100% local, raw content, compatible API v2, sans solveur commercial". Positionnement : "pas de quota, pas d'API key, suffit pour 80% des sites".

Mon conseil : **Phase A d'abord** (1-2h de ton temps, zéro coût). Si ça marche, crw-shield devient un vrai outil anti-bot. Si ça marche pas, on a un signal clair que même avec HITL c'est pas trivial.
