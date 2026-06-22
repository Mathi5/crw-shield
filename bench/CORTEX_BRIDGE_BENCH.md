# Cortex-Bridge Bench — référence

**Date** : 2026-06-21 (CEST, Paris)  
**But** : Valider si cortex-bridge débloque plus de sites durs que crw-shield sur la même VM (IP résidentielle FAI français).

## Méthodologie

- **Source** : https://forgejo.cyrleb.dev/CyrilLeblanc/cortex-bridge (clone git, branche master)
- **Stack** : Rust (axum + chromiumoxide 0.9) + 2 binaires Go (html-to-md, tls-impersonate-proxy) + Chromium 149
- **Build** : Docker multi-stage (rust 1.88 + golang 1.24), ~12 min compile
- **Lancement** : `docker run -d --rm -p 3001:3000 cortex-bridge-test:bench`
- **API** : `POST /v1/scrape` (format Firecrawl v1 : `{"url", "formats", "onlyMainContent", "timeout", "waitFor"}`)
- **Panel** : 7 sites (5 durs + 2 contrôles), même panel que crw-shield

## Résultats bruts

| Site | crw-shield (sans FS) | cortex-bridge | Delta |
|---|---|---|---|
| rust-lang | ✅ 3368 chars | ✅ 3282 chars | = |
| github.com/Mathi5/crw-shield | ✅ 5226 chars | ✅ 100000 chars (cap) | **cortex-bridge** |
| **Etsy** | ❌ 0 chars (DataDome) | ❌ 0 chars (timeout 60s, DataDome) | = bloqué |
| **Leboncoin** | ❌ 0 chars (DataDome) | ❌ 504 (DataDome) | = bloqué |
| **Amazon** | ✅ 133319 chars | ✅ 100000 chars (cap) | = |
| **LinkedIn** | ✅ 16952 (login wall) | ✅ 17486 (login wall) | = |
| **StackOverflow** | ❌ 365 (Cloudflare IUAM) | ✅ 25201 (vrai contenu) | **cortex-bridge débloque** |

**Score** : crw-shield 11/20 (55%) sur bench 20 sites, cortex-bridge **5/7 (71%) / 3/5 (60%) hard sites** sur bench 7 sites.

## Le gap technique comblé par cortex-bridge

Cortex-bridge intègre 3 pièces que crw-shield n'a PAS :

### 1. **tls-impersonate-proxy** (Go, bogdanfinn/tls-client)

Proxy MITM localhost:7890 que Chromium utilise via `--proxy-server=http://127.0.0.1:7890`. Le proxy :
- Génère une CA locale persistante (`ca-dir/`)
- Reçoit le CONNECT target:443 de Chrome
- Présente un cert signé par la CA, décrypte le tunnel
- **Ré-émet la requête vers la vraie cible via tls-client avec un profile de fingerprint TLS** (chrome_120, chrome_117, firefox_117, safari_16_0, etc.)
- Stream la réponse re-encryptée à Chrome

**Résultat** : Chrome parle TLS avec fingerprint parfait, pas son BoringSSL vanilla détectable.

### 2. **Profile pool cohérent** (UA + TLS + Sec-Ch-Ua + uaData alignés)

7 profiles "identités" stables (chrome-120, chrome-124, chrome-131, firefox-123, etc.) où **toutes** les composantes bougent ensemble : UA string, TLS profile, Sec-Ch-Ua headers, navigator.userAgentData brands, canvas noise seed, profile dir suffix.

**Pourquoi** : un anti-bot qui voit Chrome 120 UA + Chrome 124 TLS = trivialement détectable. La cohérence UA ↔ TLS ↔ Client Hints est le point clé.

### 3. **Rotation réactive L0→L1→L2→L3**

```
L0 Accept     → page OK, retourner tel quel
L1 ClearRetry → 1er block: clear cookies + storage, retry (~1s, pas de restart)
L2 Rotate     → 2e block: 15s cooldown + kill TLS proxy + spawn nouveau profile + kill Chromium + fresh profile dir (~30-45s)
L3 Fail       → après 3 rotations sur le même host, give up (HTTP 403)
```

**Compteurs per-host** : une rotation sur Leboncoin n'épuise pas le budget d'Amazon.

## Le plafond non comblé

Etsy et Leboncoin restent **bloqués pour les deux stacks** = block IP-level (DataDome + Akamai blacklisteraient l'IP résidentielle française depuis une box perso). Pour passer, il faut :
- **Proxy résidentiel rotatif** (Bright Data, Smartproxy — 50-200€/m)
- **Solveur commercial** (Scrapfly, Zyte, Browserbase — 50-500€/m)
- **Combiné** (ce que fait probablement cortex-bridge en prod, 200-500€/m)

## Recommandation pour crw-shield

**Intégrer le pattern tls-impersonate-proxy** :
- Court terme : builder le binaire Go `tls-impersonate-proxy` et le lancer en sidecar dans le container crw-shield
- Moyen terme : porter la rotation L0-L3 dans `crates/antibot/src/rotation.rs`
- Long terme : si besoin de passer Etsy/Leboncoin, payer un proxy résidentiel ou solveur commercial

**Coût estimé** : 1-2 jours de dev pour le proxy sidecar + la rotation.

## Stockage

- VM : 49G total, 13G libre après cleanup (73% used)
- Build cortex-bridge : 12 min, image finale 1.3GB (puis supprimée)
- Cleanup : `docker rmi cortex-bridge-test:bench` libère 1.3GB
- Code source cortex-bridge : `/home/moi/projects/hermes/cortex-bridge-test/cortex-bridge/` (828K, gardé pour référence)
