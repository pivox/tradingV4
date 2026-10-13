# tradingV4 — Matrice exchanges spot (étape 2)

Date : 2026-10-13, révisée le même jour après vérification des points « à valider ». Périmètre : crypto spot, résident français, horizons scalping / intraday / swing (timeframes 1m à 1d).

Légende : **vérifié** = lu sur une page officielle de l'exchange ou du régulateur ; **secondaire** = presse spécialisée ; **non vérifié** = je n'ai pas pu atteindre la source (le conteneur dans lequel je travaille bloque les appels directs aux API des exchanges, et certaines docs sont rendues en JavaScript).

## 1. Fait structurant : Binance n'est plus utilisable

Depuis le 1er juillet 2026, la période transitoire MiCA est close et seules les plateformes agréées CASP peuvent servir des clients français. Binance France SAS n'a pas obtenu l'agrément : plus d'ordres spot, bots désactivés, **autorisations API révoquées pour les nouveaux ordres spot et margin**, retraits toujours possibles ; l'AMF a déclaré l'enregistrement PSAN caduc le 2 juillet 2026 ([Cryptoast](https://cryptoast.fr/binance-quitte-france-cryptos/), [CryptoPulseInfo](https://cryptopulseinfo.com/psan-mica-etat-lieux-post-1-juillet-2026/)).

Conséquence : Binance est exclu de l'exécution. Ses archives `data.binance.vision` (ZIP mensuels de klines, sans compte) restent une source de données de recherche possible, mais **non vérifié** depuis ici que l'accès n'est pas géobloqué pour la France : un `curl -I https://data.binance.vision/data/spot/monthly/klines/BTCUSDT/1m/BTCUSDT-1m-2026-08.zip` depuis ta machine tranche la question. Dans tous les cas, les prix Binance ne sont pas ceux du lieu d'exécution : recherche large seulement, pas backtest final.

## 2. Statut réglementaire des candidats

| Exchange | Agrément MiCA (entité, régulateur) | Accessible FR | Niveau |
|---|---|---|---|
| **OKX** | OKX/OKcoin Europe Ltd, MFSA Malte, 27 janvier 2025 | Oui | secondaire, 3 sources concordantes ([CryptoActu](https://cryptoactu.com/guides/meilleurs-exchanges-france/), [CoinAcademy](https://coinacademy.fr/academie/apres-binance-quelle-plateforme-crypto-agreee-mica-choisir-france/), [Freenance](https://freenance.io/crypto/mica-licensed-crypto-exchanges-2026-eu-list-binance-kraken-coinbase-bitvavo-bitpanda-status-tracker/)) |
| **Kraken** | Payward Europe, Irlande (CBI) et Luxembourg (CSSF) selon la source | Oui | secondaire, 2 sources |
| **Coinbase** | Coinbase Luxembourg S.A. (CSSF, juin 2025) ou Coinbase Europe Ltd (CBI) selon la source | Oui | secondaire, 2 sources |
| **Bitstamp** | Bitstamp Europe SA, CSSF Luxembourg, T4 2024 | Oui | secondaire, 2 sources |
| **Bitvavo** | Bitvavo B.V., AFM Pays-Bas, T4 2024 | Oui | secondaire |
| **Bybit** | Bybit EU, FMA Autriche, mai 2025, 29 États de l'EEE | Oui (EEE) | secondaire ([Cointelegraph](https://cointelegraph.com/news/bybit-secures-mica-license-establishes-eu-hq-in-vienna)) |
| Crypto.com | Foris MT, MFSA Malte ; statut noté « partiel » par une source | Oui | secondaire |
| Bitget | Non (VASP Lituanie) | Risqué | secondaire |
| KuCoin | Non couvert par les sources lues | non vérifié | |
| Binance | Non | **Non** | voir §1 |
| Hyperliquid (DEX) | Hors MiCA ; pas de géoblocage France/UE (restrictions : États-Unis, Ontario, territoires sanctionnés) ; débat ouvert sur le périmètre MiCA/MiFID pour les perps on-chain, le spot n'est pas visé explicitement | Techniquement oui, cadre juridique non tranché | secondaire ([Coinperps](https://www.coinperps.com/learn/hyperliquid-restricted-countries), [FinTelegram](https://fintelegram.com/mica-mifid-ii-perimeter-radar-hyperliquid-and-the-eus-on-chain-perps-problem/)) |

Avant toute ouverture de compte, vérifier le nom exact de l'entité dans le registre ESMA ou la liste AMF : les noms diffèrent selon les sources pour Kraken et Coinbase.

## 3. Matrice fonctionnelle (données + exécution spot)

| Critère | OKX | Kraken | Coinbase Advanced | Bitstamp | Bitvavo | Hyperliquid spot |
|---|---|---|---|---|---|---|
| Historique OHLCV en masse (fichiers) | Non | **Oui, vérifié** : CSV OHLCVT complets depuis le premier trade de chaque paire jusqu'au 30 juin 2026, mises à jour trimestrielles (`Kraken_OHLCVT_2026Q2.zip`), intervalles 1/5/15/30/60/240/720/1440 min, 5 parts d'environ 2 Go, SHA-256 publié, sans en-tête, bougies sans trade absentes ([Kraken](https://support.kraken.com/gb/articles/360047124832-downloadable-historical-ohlcvt-open-high-low-close-volume-trades-data)) | Non | Non | Non | Non |
| Historique OHLCV via API | `/market/candles` : les dernières ~24 h en 1m (1 440 bougies), 300/req ; `/market/history-candles` : **100/req**, remonte **jusqu'en 2021**, 20 req/2 s par IP (secondaire : [dccd](https://download-crypto-currencies-data.readthedocs.io/en/v2.3.3/histo_dl.okx.html), [nautilus](https://nautilustrader.io/docs/core-latest/src/nautilus_okx/http/query.rs.html), [skillselion](https://skillselion.com/skills/okx/agent-trade-kit/okx-cex-market)). Ordre de grandeur : 1 an de 1m = 5 256 requêtes, environ 10 min à 10 req/s | **720/req, vérifié**, pagination `since`, 1 req/s recommandé, pas de dump via API ([Kraken](https://docs.kraken.com/exchange/guides/general/historical-data)) | **350/req, vérifié**, granularités 1m, 5m, 15m, 30m, 1h, 2h, 4h, 6h, 1d ([Coinbase](https://docs.cdp.coinbase.com/api-reference/advanced-trade-api/rest-api/public/get-public-product-candles.md)) | `step` en secondes ; plafond global **8 000 req/10 min, vérifié** ([Jentic/OpenAPI](https://jentic.com/apis/bitstamp.net/bitstamp.md)) ; limite par requête non vérifiée (1 000 d'après ma connaissance) | **1 000 points/min, vérifié**, REST + WS confondus, par IP sans auth ([Bitvavo](https://docs.bitvavo.com/docs/rate-limits/)) ; limite par requête non vérifiée (1 440 d'après ma connaissance) | `candleSnapshot` : **seules les 5 000 dernières bougies sont disponibles, vérifié**, soit 3,5 jours en 1m et 13,7 ans en 1d ; intervalles 1m à 1M ; spot nommé `PURR/USDC` ou `@index` ([Hyperliquid](https://hyperliquid.gitbook.io/hyperliquid-docs/for-developers/api/info-endpoint)) |
| Trades historiques | API, « plusieurs mois » par curseur (secondaire) | API 1 000/req depuis le début, vérifié | API | API | API | API, profondeur limitée |
| WebSocket public | Oui ; 3 connexions/s par IP, 480 subscribe/h par connexion, vérifié ([OKX](https://www.okx.com/docs-v5/en/)) | Oui (v2) | Oui | Oui | Oui, compte dans les 1 000 points/min | Oui |
| Ordres spot via API | Oui ; V3 a un client REST/WS OKX (perp) réutilisable en référence | Oui | Oui | Oui | Oui | Oui (signature EIP-712 ; V3 a un signer) |
| Frais spot de base (maker / taker) | **Tranché : 0,20 % / 0,35 % « Regular » sans compte dérivés, définitif** (§7 ; décision du 13/10 (18:35) : pas de compte X-Perps). La page officielle OKX Europe, lue le 13/10 (08 §B.2), réserve 0,08 % / 0,10 % aux comptes X-Perps ouverts : grille écartée par décision, pas « à vérifier ». Sources lues avant cette page : 0,08 % / 0,10 % « Regular » avec la mention « may vary depending on your location » ([BitDegree, août 2026](https://www.bitdegree.org/crypto/tutorials/okx-fees)) contre 0,20 % / 0,35 % « standard en Europe » ([CoinAcademy](https://coinacademy.fr/academie/apres-binance-quelle-plateforme-crypto-agreee-mica-choisir-france/), article en partenariat commercial avec OKX). La page officielle, rendue en JS, était illisible d'ici à la première rédaction de cette ligne. Les X-Perps sont des futures MiFID à 5 ans d'OKX Europe (Malte) ([Cointelegraph](https://cointelegraph.com/news/okx-europex-perps-regulated-crypto-derivatives)). **Seule vérification restante : la page « Frais » du compte doit afficher Regular 0,20 % / 0,35 %.** | **0,40 % / 0,80 %, vérifié** au palier 1 de Kraken Pro (0 à 10 k$ / 30 j) ; 1 % sur l'interface simple ([Kraken](https://www.kraken.com/features/fee-schedule)) | 0,40 % / 0,60 % (< 10 k$/mois), secondaire | 0,30 % / 0,40 %, secondaire | **Vérifié** : 0,15 % / 0,25 % sur les marchés EUR, **0,05 % / 0,05 % sur les marchés USDC** ([Bitvavo](https://bitvavo.com/en/fees)) | **0,040 % / 0,070 %, vérifié**, palier 0 ([Hyperliquid](https://hyperliquid.gitbook.io/hyperliquid-docs/trading/fees)) |
| Univers spot | Large (USDT/USDC/EUR) | Large, EUR natif | Large, EUR natif | Moyen, EUR natif | Moyen, EUR et USDC | Étroit (tokens natifs HYPE, PURR…), liste via `spotMeta`, non chiffré |
| Sandbox / démo | Demo trading, déjà utilisé par V3 | **Vérifié** : « Spot UAT » sur demande auprès d'un account manager ; le sandbox libre-service ne couvre que les futures ([Kraken](https://docs.kraken.com/home/guides/quickstart)) | **Vérifié** : sandbox sans auth, réponses statiques figées, comptes et ordres seulement, **aucune donnée de marché** ([Coinbase](https://docs.cdp.coinbase.com/coinbase-app/advanced-trade-apis/sandbox)) | non vérifié | Non | Testnet |

## 4. Ce que ces vérifications changent

1. **Le scalping dépendait d'une seule donnée alors inconnue : les frais OKX réels de ton compte** (tranché en §7 : 0,20/0,35 %, définitif le 13/10 (18:35)). À 0,08/0,10 % le scalping aurait été envisageable ; à 0,20/0,35 % il ne l'est pas sur un CEX, et la seule alternative à frais bas pour un résident français devient Bitvavo sur les paires USDC (0,05/0,05 %, vérifié), avec un univers plus étroit et un rate limit serré (1 000 points/min, WS compris).
2. **Hyperliquid spot est inutilisable comme source d'historique** (5 000 bougies max), et son univers spot est constitué de tokens natifs : à écarter pour V4, sauf comme lieu d'exécution ultérieur.
3. **Kraken reste la seule source d'historique complet et gratuit**, mais ses frais spot au palier d'entrée (0,40/0,80 %) l'excluent de l'exécution pour tout ce qui n'est pas du swing.
4. **L'API OKX suffit pour reconstruire plusieurs années de 1m** à un rythme raisonnable (100 bougies/req, 10 req/s), à condition d'un ingesteur incrémental avec reprise : c'est ce que V3 n'avait pas.
5. Aucun candidat n'offre de sandbox avec données de marché réelles : le paper trading sur websocket public reste la seule « démo » fidèle, et c'est un module à construire, pas à acheter.

## 5. Mode « sans exchange »

1. **Paper trading sur flux réel** (défaut V4) : websocket public d'un exchange, exécution simulée avec frais et slippage paramétrés, même interface `Strategy` que le backtest. Aucun compte requis.
2. **Backtest sur données historiques** : jalon 1.
3. **DEX** : Hyperliquid spot ou AMM, en option après le live CEX.

## 6. Recommandation révisée

- **Exécution : OKX spot**, grille 0,20/0,35 % (§7). Agrément MiCA Malte, démo existante, client V3 réutilisable en référence, historique API suffisant.
- **Repli exécution si frais OKX ≥ 0,20 % : Bitvavo, paires USDC** pour le scalping/intraday ; Kraken ou OKX pour le swing.
- **Données de backtest : dumps Kraken OHLCVT** (socle multi-années, checksum) + **API OKX history-candles** (prix du lieu d'exécution, derniers mois jusqu'à 2021 si besoin). Le biais Kraken/OKX se mesure sur la période de recouvrement.
- **Interface d'exchange dès le squelette**, deux implémentations au jalon 1 : `KrakenHistoricalSource` (fichiers) et `OkxMarketData` (REST + WS). Pas d'ordres au jalon 1.

## 7. Décision du 2026-10-13 (Haythem)

**Frais OKX retenus : 0,20 % maker / 0,35 % taker.** Conséquences :

- **OKX reste l'exchange d'exécution** pour l'intraday et le swing. Avec un aller-retour à 0,70 % en taker (0,40 % en maker), une stratégie doit viser un gain brut moyen par trade nettement supérieur à 1 % pour rester positive : le moteur de backtest doit rejeter par défaut toute configuration dont l'expectancy nette est négative après frais, et privilégier les ordres limit (maker).
- **Le scalping n'est pas viable sur OKX à ces frais.** Il n'est pas abandonné : il est conditionné à un second connecteur d'exécution, **Bitvavo paires USDC (0,05/0,05 %)**, qui n'entre pas dans le jalon 1. Le moteur de backtest devra donc accepter une grille de frais par exchange dès le départ, pour comparer la même stratégie sur OKX et sur Bitvavo.
- Le `curl` vers `data.binance.vision` reste à faire depuis ta machine quand on écrira l'ingesteur ; il n'influence pas l'architecture.

**Décision du 13/10 (18:35) : spot uniquement, réaffirmé** (« pas de perpetual, que du spot »). Pas de compte dérivés OKX (X-Perps), même sans trader de dérivés : la grille 0,20 % / 0,35 % (`okx_regular`) est définitive, la grille 0,08 % / 0,10 % réservée aux comptes X-Perps est écartée par décision, pas « à vérifier ». Seule vérification restante : la page « Frais » du compte doit afficher Regular 0,20 % / 0,35 %.
