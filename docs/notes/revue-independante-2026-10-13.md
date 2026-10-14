# Revue indépendante de la conception tradingV4 (13/10/2026, soir)

Date : 2026-10-13, après la passe USDC du thread Plan (PR #9, commit `c1dd588`). Thread « Revue critique du plan tradingV4 ». Périmètre : documents 01 à 08, notes, `CLAUDE.md`, issues S1 à S8. Méthode : relecture complète, calculs refaits, faits datés relus sur les pages officielles, API publiques d'OKX et de Kraken interrogées le 13/10. Les décisions du propriétaire ne sont pas rediscutées : ce qui les touche est posé en question (§4), pas modifié.

## 1. Verdict

La conception est cohérente, chiffrée et traçable ; les décisions du 13/10 (spot seul, OKX 1d/4h en USDC, 300 € et pilote 20/50/100 %, pas de X-Perps, verdict avant impôt, seuil G0 à 300, Parquet seul, Rust pur, jalon 1 complet) sont reportées partout sans contradiction. Aucune erreur trouvée ne renverse une décision. Deux faits nouveaux, mesurés sur l'API, touchaient le pilote et l'univers live ; le propriétaire les a tranchés le 14/10 (05:21) en suivant les options recommandées (§4), et les documents 04 et 08 portent ces décisions datées. Le reste est corrigé dans une PR distincte (§3).

## 2. Ce qui est re-validé

| Point | Documents | Source relue le 13/10 | Verdict |
|---|---|---|---|
| USDT non échangeable pour un résident de l'EEE chez OKX Europe | 01 §5, 02 §8, 04 §10 | [OKX Europe, 17/07/2026](https://www.okx.com/en-eu/learn/usdt-one-way-convert) ; [avis ESMA du 08/10/2026](https://www.esma.europa.eu/press-news/esma-news/esma-sets-out-supervisory-expectations-services-related-unauthorised) | exact |
| Grille Regular 0,20 / 0,35 % sans X-Perps, 0,08 / 0,10 % avec | 02 §7, 04 §3, 08 B.2 | [page OKX Europe, maj 03/09/2026](https://www.okx.com/en-eu/help/what-are-the-new-trading-fees-for-eea-users) | exact |
| Compte spot seul à 0,10 / 0,20 % depuis le 25/09/2026, stablecoins 0,00 / 0,05 % | 02 §8, 04 §3, 08 B.2 | [avis OKX du 11/09/2026](https://www.okx.com/en-eu/help/important-notice-upcoming-spot-fee-adjustment-eea) | exact ; grille du compte à confirmer (point ouvert du thread Plan) |
| Domaines EEE, 20 IP par clé, suppression à 14 jours, erreur 50119 | 02 §8, 03 §11 S7, 08 C.5 | [FAQ API OKX](https://www.okx.com/en-eu/help/api-faq) ; table des sites de la doc officielle (`eea.okx.com`, `wseea.okx.com:8443`, démo `wseeapap.okx.com:8443`) | exact |
| BTC-USDC coté depuis le 21/08/2025, `tradeQuoteCcyList` USDG/USD/USDC/RLUSD | 02 §8, 04 §2.1 | `GET /api/v5/public/instruments` : `listTime` 1755763200000 = 21/08/2025 08:00 UTC pour BTC, ETH, SOL, XRP, DOGE | exact |
| Barre `1D` à 16:00 UTC, `1Dutc` à 00:00 UTC | 02 §8, 03 S7, 04 §2.1 | `GET /api/v5/market/candles` : `1D` ts 1791907200000 = 13/10 16:00 UTC ; `1Dutc` ts multiples de 86 400 s ; doc officielle (`1Dutc` = UTC+0) | exact |
| Volumes USDC (moyennes 14 j : BTC 38,6 M$, ETH 18,7, SOL 6,8, XRP 4,7, DOGE 0,95) | 02 §8, 04 §3.1, 08 B.1 | `GET /api/v5/market/ticker` le 13/10 (24 h) : BTC 49,0 M$, ETH 20,8, SOL 10,8, XRP 3,9, DOGE 0,78 | cohérent ; 0,5 % de la minute moyenne = 134 / 65 / 24 / 16 / 3 $, recalculé |
| Binance : ordres spot, bots et API révoqués en France au 01/07/2026, retraits maintenus | 02 §2 | Cryptoast du 24/06/2026 (secondaire), caducité AMF 02/07/2026 | exact |
| Agrément MiCA d'OKX Europe (Malte, 27/01/2025) | 02 §3 | OKX / presse | exact |
| Kraken : dumps OHLCVT jusqu'au 30/06/2026 ; Kraken Pro 0,40 / 0,80 % ; Bitvavo 0,15 / 0,25 % EUR et 0,05 / 0,05 % USDC | 02 §3 | pages officielles | exact |
| Livret A 1,7 % depuis le 1er août 2026 | 04 §1.1 | [info.gouv.fr](https://www.info.gouv.fr/actualite/augmentation-du-taux-du-livret-a-a-compter-du-1er-aout-2026) | exact, « à valider » levé |
| PFU 31,4 % (12,8 % + 18,6 %) | 06 §3.1 | [impots.gouv.fr, page modifiée le 17/07/2026](https://www.impots.gouv.fr/particulier/questions/comment-declarer-les-plus-ou-moins-values-sur-cessions-dactifs-numeriques) | exact, « à valider » levé ; economie.gouv.fr (02/10/2025) affiche encore 30 %, page périmée |
| Crates : `temporalio-sdk` 1.1.0, `arrow`/`parquet` 60.0.0, `clap` 4.6.7, `ureq` 3.4.2, `zip` 9.0.2 | 03 §3.1, §14.1 | crates.io | exacts ; `sqlx` 0.9.0 date du 21/05/2026, pas du 30/09 (corrigé) ; Rust stable courante 1.99.0 (01/10/2026), l'étude est en 1.97 (note ajoutée) |
| `history-candles` : `limit=300`, 20 req/2 s, `before` seul = plus récentes | 02 §3, 03 §11 S7, §14.1, issue S7 | doc officielle du site EEE ([dépôt `okx/ai-builder-openapi-md`](https://github.com/okx/ai-builder-openapi-md), généré le 23/09/2026) et essai : 300 bougies renvoyées, 299 minutes d'écart | exact, « à valider » levé : 1 752 requêtes par an au lieu de 5 256 |
| Calculs de 04 et 08 : n_min = max(200 ; (2σ/μ)²), seuil 1,5 × n_min = 300 (1d) / 450 (4h), Sharpe de bruit √(2 ln N / T), écarts-types et puissance, lignes 35 € (1d) / 75 € (4h), coût/stop ≤ 0,10 R, hurdle 6,4 % ≈ 19 €/an, échantillon F3 ≈ 85-110 trades, calendrier (fin du jalon 1 entre le 15/12/2026 et le 12/01/2027, verdict ≈ mois 27-34 = janvier à août 2029), probabilités de séries de pertes | 04 §1 à §7, 08 A et B | recalculés (script dans le thread) | exacts |

## 3. Ce qui est corrigé (PR distincte sur `docs/`)

1. **Paire Kraken de recherche : XBTUSD, pas XBTUSDT.** Sur Kraken, XBTUSD est coté depuis le 06/10/2013 et XBTUSDT seulement depuis le 20/12/2019 (API publique `Trades`, premier trade) : XBTUSDT ne couvre pas le bloc DEV 2018-2022. L'USD est en plus la meilleure approximation de l'USDC du lieu d'exécution. 03 §0, §5 (arborescence, dataset `btcusd_2y`), §8, §11 S2 et S8, §12, §14.1 ; 04 §10 ; issues S2, S5, S8.
2. **`history-candles` : `limit=300` accepté, 20 req/2 s par IP, `1Dutc` documentée** (doc officielle + essai). 02 §3, 03 §8, §11 S7, §12, §14.1 ; issue S7. Reste à valider depuis le poste : la sémantique de `after` et `before` combinés.
3. **Minimums d'ordre des paires USDC**, lus sur l'API publique (à confirmer par `GET /api/v5/account/instruments`) : BTC-USDC `minSz` 0,0001 BTC ≈ 8,3 $ (≈ 7,3 €), ETH-USDC 0,001 ETH ≈ 2,4 $, SOL 0,01, XRP 1, DOGE 10 ; sur BTC et ETH c'est dix fois le minimum des paires USDT. Les phrases « ≤ 2,3 USDT, à relire sur les paires USDC » et « ne contraignent aucune paire » sont remplacées ; conséquence pour le palier 20 % en §4. 04 §3.1 point 6, §6.2, §6.3, §9, §10 ; 08 B.1, B.2, B.3.
4. **04 §7 : F4 est en 4h maker**, comme 08 A.3 et A.6, pas en 1d.
5. **04 §5 G1 : « ≥ 20 trades/an » devient « ≥ 20 trades par année non neutre »**, sinon la règle contredit l'année neutre (filtre rouge, zéro trade) introduite dans la même cellule.
6. **04 §2.3 : embargo = 2 × détention maximale**, la chauffe des indicateurs n'entre pas dans le max (sinon l'embargo vaudrait 200 jours, pas 60).
7. **04 §7 : 54 M de bougies × 6 colonnes en float32 ≈ 1,3 Go**, pas 0,9 Go (2,6 Go en f64 était juste).
8. **04 §3.1 : 1 USDC ≈ 0,87 €** (1,15 $/€, comme 08 B.1), pas 0,85 € ; la marge pessimiste est de 13 %, pas 15 %.
9. **Fiscalité : terme exact.** Les échanges crypto ↔ crypto, stablecoins compris, sont en « sursis d'imposition » (impots.gouv.fr), pas « non imposables » ; le « à valider » de 04 §1.1, §10, 06 §3.1 et 08 B.5, D est levé avec la source.
10. **03 §3.1 : `sqlx` 0.9.0 publié le 21/05/2026** ; note sur Rust 1.99.0 stable en §14.1.
11. **04 §1.1 : Livret A 1,7 % vérifié** avec la source.

Les notes `okx-bybit-2026-10-13.md` et `raspberry-pi-machine-dediee.md` sont cohérentes avec 02, 03 et 08 ; rien à corriger.

## 4. Tranché par le propriétaire le 14/10 (05:21)

1. **Palier 20 % et minimum BTC-USDC.** À 60 € alloués et 0,60 € de risque, la ligne BTC vaut ≈ 7 € à stop 8,5 % ; le minimum BTC-USDC vaut ≈ 7,3 € à 83 400 $ et ≈ 8,7 € à 100 000 $. La ligne de repli à risque 0,5 % (≈ 3,5 €) ne passe pas, et ETH-USDC (≈ 2,1 €) est à la limite à ce risque. Options : (a) porter la ligne BTC au minimum avec un risque effectif plafonné à 1,5 % du capital alloué, recommandé parce que 60 € × 1,5 % = 0,90 € et que la Monte Carlo de G2 tolère 0,5 à 1 % ; (b) premier palier à 30 % (90 €) ; (c) palier 20 % sans BTC. **Réponse du 14/10 (05:21) : (a)**, reportée dans 04 §3.1 point 6, §6.2, §6.3, §9 point 6, §10 (décision 6) et 08 B.1, B.3, D.
2. **Univers live sur les carnets USDC.** Avec la règle « notional ≤ 0,5 % du volume quote de la minute », seuls BTC et ETH passent une ligne de 35 € en 1d, et BTC seul une ligne de 75 € en 4h maker (02 §8, 04 §6.2, mesuré par le thread Plan, cohérent avec les volumes relus). Conséquences écrites nulle part en une phrase : « ≤ 4 positions » devient « ≤ 2 », F3 top-2 ne classe plus rien, et les 8 à 36 trades par an comptés sur 4 à 6 paires deviennent ≈ 4 à 15 ; la clause « < 30 trades à 6 mois » du paper et de G6 jouera presque sûrement. Options : (a) accepter cet univers pour le cycle 1 (F1 sur BTC et ETH), recommandé parce que le protocole le prévoit déjà ; (b) porter la règle à 1 % du volume médian 1m ; (c) ajouter les carnets EUR, 2 à 6 fois plus minces. **Réponse du 14/10 (05:21) : (a)**, reportée dans 04 §6.2 (ligne Univers), §10 (décision 6) et 08 B.4, D ; la mesure des médianes 1m (04 §3.2) reste due avant G0.
3. **Grille de frais du compte** (0,10 / 0,20 ou 0,20 / 0,35 %) : déjà demandée par le thread Plan ; rien n'est recalculé tant que le compte ne l'a pas affichée. Réponse du 14/10 (05:21) dans le thread Plan : « 0,2 », lue par ce thread comme la grille 0,20 / 0,35 % (baseline inchangée) ; la lecture « taker 0,20 % de la grille 0,10 / 0,20 » reste possible, à confirmer par le propriétaire.

## 5. Relevés bruts du 13/10 (API publiques, `www.okx.com` ; les bougies et instruments publics sont identiques sur `eea.okx.com` selon le thread Plan)

| Instrument | `listTime` | `minSz` | `lotSz` | `tickSz` | Dernier prix | minSz × prix | Volume 24 h (quote) |
|---|---|---|---|---|---|---|---|
| BTC-USDC | 21/08/2025 08:00 UTC | 0,0001 BTC | 1e-8 | 0,1 | 83 388,6 $ | 8,34 $ ≈ 7,25 € | 48,96 M$ |
| ETH-USDC | idem | 0,001 ETH | 1e-6 | 0,01 | 2 423,21 $ | 2,42 $ | 20,85 M$ |
| SOL-USDC | idem | 0,01 SOL | 1e-5 | 0,01 | 106,91 $ | 1,07 $ | 10,84 M$ |
| XRP-USDC | idem | 1 XRP | 1e-3 | 0,0001 | 1,3689 $ | 1,37 $ | 3,93 M$ |
| DOGE-USDC | idem | 10 DOGE | 1e-2 | 0,00001 | 0,08458 $ | 0,85 $ | 0,78 M$ |

Autres relevés : `history-candles?instId=BTC-USDT&bar=1m&limit=300` renvoie 300 bougies (premier `ts` 1791390900000, dernier 1791372960000, 299 minutes d'écart) ; `candles?bar=1D` renvoie des `ts` à 16:00 UTC, `bar=1Dutc` à 00:00 UTC ; Kraken `Trades?pair=XBTUSDT&since=0` : premier trade 1576778778 = 20/12/2019 ; `pair=XBTUSD` : premier trade 1381095256 = 06/10/2013. Conversion à 1,15 $/€.

## Sources

- OKX Europe, USDT : https://www.okx.com/en-eu/learn/usdt-one-way-convert
- OKX, avis du 11/09/2026 sur les frais spot EEE : https://www.okx.com/en-eu/help/important-notice-upcoming-spot-fee-adjustment-eea
- OKX, grille de frais EEE (maj 03/09/2026) : https://www.okx.com/en-eu/help/what-are-the-new-trading-fees-for-eea-users
- OKX, FAQ API : https://www.okx.com/en-eu/help/api-faq
- OKX, doc V5 en Markdown (sites, `market/history-candles`, `account/instruments`, `account/trade-fee`) : https://github.com/okx/ai-builder-openapi-md
- ESMA, 08/10/2026 : https://www.esma.europa.eu/press-news/esma-news/esma-sets-out-supervisory-expectations-services-related-unauthorised
- Kraken, API publique `Trades` : https://docs.kraken.com/api/docs/rest-api/get-recent-trades
- info.gouv.fr, Livret A au 1er août 2026 : https://www.info.gouv.fr/actualite/augmentation-du-taux-du-livret-a-a-compter-du-1er-aout-2026
- impots.gouv.fr, plus-values sur actifs numériques (modifié le 17/07/2026) : https://www.impots.gouv.fr/particulier/questions/comment-declarer-les-plus-ou-moins-values-sur-cessions-dactifs-numeriques
- crates.io : https://crates.io/crates/sqlx, https://crates.io/crates/clap, https://crates.io/crates/ureq, https://crates.io/crates/zip
