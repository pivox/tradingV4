# tradingV4 — Cadrage (étape 1)

## 0. Objectif final (Haythem, 2026-10-13)

**Livrer un système capable de trader réellement et de dégager un PnL net positif.** Tout le reste (backtest, paper, architecture) est un moyen. Conséquence : le plan ne s'arrête pas au moteur ; il comprend une phase de recherche de stratégies avec des règles anti-illusion (coûts nets dès le backtest, holdout jamais touché, walk-forward, seuil minimum de trades avant conclusion), puis un passage obligatoire par le paper trading sur flux réel avant le premier euro en live. V3 est l'exemple à ne pas reproduire : beaucoup d'outillage, aucune configuration rentable trouvée.

Date : 2026-10-13. Source : lecture du dépôt `pivox/tradingV3` (README, `CLAUDE.md`, `agents.md`, `docs/roadmap/`, `docs/handbook/recap-2026-10-08-okx-hyperliquid.md`, `docs/handbook/backtesting-okx-hl.md`, `backlog/techlead/`, inventaire de `trading-app/src`).

## 1. Ce qu'est tradingV3 aujourd'hui

- Plateforme **futures perpétuels** OKX + Hyperliquid (Bitmart retiré le 2026-10-08). Mainnet jamais activé ; OKX limité à la démo, Hyperliquid au testnet avec preuve HMAC.
- Monolithe Symfony 7.1 / PHP 8.3 : ~1 460 fichiers PHP, ~225 000 lignes, 529 fichiers de tests, PHPStan niveau 6 avec 1 832 erreurs héritées. Plus un orchestrateur FastAPI (~25 000 lignes), un cron Temporal, un signer Hyperliquid, un front React CRA (service commenté dans `docker-compose.yml`).
- Boucle MTF chaque minute sur 100+ symboles : pour chaque symbole × timeframe, appel REST (250 à 490 klines), recalcul des indicateurs en PHP, pas de persistance des klines (`OkxMarketDataGateway::saveKlines` lève `notImplemented`), pas de cache, 429 fréquents.
- Deux systèmes de stratégies coexistent : legacy (`validations.<mode>.yaml` + `trade_entry.<mode>.yaml`, encore utilisé par le MTF live) et moderne (`config/trading/` : mode/setup contracts versionnés et hashés, TradingCore). Le branchement runtime du moderne est incomplet.
- Backtest = replay Paper d'une capture figée de 24 h, dataset entier à chaque fois, **46 à 103 min par cellule**. Pas de fenêtre bornée, pas de walk-forward (BSM-005, WFO-002 non commencés). Un runtime Backtrader existe mais appelle PHP en sous-processus pour chaque règle/indicateur.
- Résultat métier : **0 trade sur les 12 cellules** certifiées, et aucune configuration rentable trouvée dans les campagnes de recherche (436 configs pré-enregistrées, aucune éligible ; swing, scalping, midcap rejetés).

## 2. Ce que V3 fait bien et que V4 reprend (idées, pas code)

- Invariants de sécurité : pas de position sans stop, kill switch, gates explicites avant tout ordre réel, aucun secret mainnet dans les phases de test.
- Configuration de stratégie **versionnée, hashée, immuable** avec replay déterministe (`config_hash`) : deux runs à hash identique sont comparables à l'octet.
- Coûts nets dès le départ : frais, spread, slippage (et funding, mais hors sujet en spot).
- Rigueur de recherche : pré-enregistrement des configs, holdout gardé, « bad trades first », seuil de trades minimum avant toute conclusion.
- Lineage run → set → trade, et une vue d'analyse unique des trades.

## 3. La piste V4 : là où V3 n'est pas allée

| Axe | V3 | V4 |
|---|---|---|
| Marché | Futures perp, levier | **Spot uniquement**, pas de levier |
| Données | Fetch REST à chaque run, rien stocké | **Base historique locale** (OHLCV + éventuellement trades/book), ingestion incrémentale, source de vérité pour backtest et live |
| Backtest | Replay 24 h, 1 h+ par cellule | **Moteur rapide** : fenêtre bornée, multi-années, walk-forward, paramétrable ; ordre de grandeur visé : secondes à minutes |
| Stratégie | Règles YAML évaluées par un moteur MTF unique, pas d'abstraction Strategy | **Interface Strategy pluggable**, même code en backtest, paper et live |
| Sans exchange | Fake exchange interne, paper sur capture | **Paper trading sur flux réel** comme mode de première classe, avant tout CEX |
| Exécution | OKX démo, HL testnet, rien en réel | Live spot sur un CEX accessible à un résident français, derrière les mêmes gates que V3 |
| Taille | Monolithe 225k lignes, CI partielle | Périmètre réduit, CI complète dès le squelette, zéro dette PHPStan au départ |

## 4. Périmètre du premier jalon

Objectif : **un backtest spot de bout en bout, reproductible et rapide**.

Inclus :
1. Ingestion d'historique OHLCV spot (un exchange, quelques paires, plusieurs timeframes) dans une base locale.
2. Une interface `Strategy` et une stratégie simple de référence (ex. croisement de moyennes) pour valider le moteur, pas pour gagner de l'argent.
3. Moteur de backtest : fenêtre de dates, frais et slippage paramétrés, pas de look-ahead, sortie de métriques (PnL net, expectancy, profit factor, max drawdown, nombre de trades, durée moyenne).
4. Export des trades et des métriques dans un format comparable entre runs (hash de config + hash de données).

Non-objectifs du jalon 1 :
- Ordres réels, même en démo.
- Futures, levier, funding.
- Dashboard ; un export CSV/JSON suffit.
- Multi-utilisateurs.
- Reprise de code V3.

## 5. Contraintes connues

- **Résident français** : seuls les CEX enregistrés PSAN/MiCA sont utilisables en spot. Candidats à vérifier en étape 2 : Binance, Kraken, Coinbase, OKX, Bitstamp (statut d'enregistrement, API spot, données historiques). Hyperliquid est un DEX avec un marché spot, à évaluer séparément. **À valider** au cas par cas, rien n'est acquis.
- **Performance** : critère de choix de stack à l'étape 3. Le calcul d'indicateurs et le backtest en PHP pur sont la cause principale de la lenteur V3 ; le choix d'un langage plus rapide pour le moteur (Python + NumPy/Polars, Rust, ou autre) se décide à l'étape 3, pas maintenant.
- **Stack non figée** : les seules hypothèses prises ici sont PostgreSQL pour le stockage et Docker pour l'exécution (stockage amendé le 2026-10-13 (21:47) : Parquet seul au jalon 1, §6 point 6).

## 6. Décisions prises les 13 et 14/10/2026

1. **Classes d'actifs** : crypto uniquement (réponse Haythem).
2. **Horizons** : scalping, intraday et swing, donc timeframes de 1m à 1d (réponse Haythem). **Amendé le 2026-10-13 (16:53)** : sur OKX à 0,20/0,35 %, seuls 1d et 4h sont recherchés (1h en maker) ; scalping et stops intraday sont gelés jusqu'à un connecteur Bitvavo USDC ([04-protocole-recherche.md](04-protocole-recherche.md) §3.3 et §10). Conséquences : le stockage doit tenir des années de bougies 1m sur plusieurs paires (ordre de grandeur : 525 600 bougies 1m par paire et par an) ; le moteur de backtest doit gérer plusieurs timeframes sur la même fenêtre, et pour le scalping les frais et le slippage pèsent plus que le signal, donc ils sont paramétrés dès le jalon 1.
3. **Données de test** : par défaut, données publiques (archives `data.binance.vision` et API REST de l'exchange retenu), pas les captures 24 h de V3, trop courtes pour du swing et liées au format Paper de V3. À revoir si tu veux réutiliser `tradingV3-private/`.
4. **Exchanges** : par défaut, une interface d'exchange dès le départ (données + ordres), avec une seule implémentation au jalon 1. L'exchange concret est choisi à l'étape 2.
5. **Spot uniquement, réaffirmé le 2026-10-13 (18:35)** (« pas de perpetual, que du spot ») : pas de compte dérivés OKX (X-Perps), même sans trader de dérivés ; la grille de frais OKX 0,20 % maker / 0,35 % taker est définitive ([02-matrice-exchanges.md](02-matrice-exchanges.md) §7). Un avis OKX du 11/09/2026, en vigueur le 25/09/2026, annonce 0,10 % maker / 0,20 % taker pour le compte spot seul, mais le compte du propriétaire affiche 0,20 % le 14/10 (05:21) (« 0,2 », lu comme le maker de la grille Regular 0,20 / 0,35 %) : la baseline 0,20 / 0,35 est maintenue et rien n'est recalculé (02 §8).
6. **Jalon 1, décidé le 2026-10-13 (21:47)** : Parquet seul pour le stockage (PostgreSQL au jalon 2 pour les métadonnées, Timescale au jalon 3 seulement si besoin), stratégies en Rust pur sans scripting, périmètre complet en 8-12 semaines à mi-temps avec point de contrôle en fin de semaine 3 ([03-architecture.md](03-architecture.md) §13).
7. **Quote d'exécution : USDC, pas d'USDT, décision du 13/10 (22:46)** (« Usdc pas usdt ») : l'USDT n'est pas échangeable pour un résident de l'EEE chez OKX Europe, donc l'exécution, le capital et les seuils passent en USDC sur les paires `XXX-USDC` (carnet USD unifié), les carnets EUR, 2 à 6 fois plus minces, étant écartés ([02-matrice-exchanges.md](02-matrice-exchanges.md) §8).
8. **Feu vert sur le code, le 14/10 (05:21)** (« 4 ok mais utilise mes agents avec les modèles adéquat ») : le jalon 1 démarre par S1, chaque tâche étant confiée à un agent dont le modèle correspond à la difficulté (`CLAUDE.md`). Le même jour, ligne BTC du palier 20 % portée au minimum d'ordre et univers live du cycle 1 réduit à BTC et ETH ([04-protocole-recherche.md](04-protocole-recherche.md) §10, décision 6).

## 7. Étapes suivantes du plan

- Étape 2 : matrice exchanges × fonctions (données, ordres spot, frais, restrictions FR, API).
- Étape 3 : architecture cible par modules, choix de stack sur critère perf.
- Étape 4 : squelette du dépôt + premier backtest de bout en bout.
