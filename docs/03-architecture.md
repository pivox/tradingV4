# tradingV4 — Architecture cible (moteur Rust)

Date : 2026-10-13, révisée le même jour après la décision du propriétaire : **le moteur est écrit en Rust, langage principal**. Entrées : [01-cadrage.md](01-cadrage.md), [02-matrice-exchanges.md](02-matrice-exchanges.md), [04-protocole-recherche.md](04-protocole-recherche.md) et [05-ihm.md](05-ihm.md) (raccordés en §15), [08-strategies-risque-exploitation.md](08-strategies-risque-exploitation.md) (décisions du propriétaire du 13/10 (18:35), reportées en §0, §2, §7.3, §12 et §15), la décision du 13/10 (22:46) du propriétaire, « Usdc pas usdt » (exécution en paires XXX-USDC chez OKX Europe : l'USDT n'y est pas échangeable pour un résident de l'EEE, page OKX Europe du 17/07/2026, et l'ESMA a demandé le 08/10/2026 aux prestataires de cesser tout service lié aux stablecoins non conformes à MiCA ; reportée en §0, §2, §5.2, §11, §12, §14 et §15), version précédente de ce document (moteur Python/Polars), trois propositions d'architecture Rust confrontées (perf-first, iteration-first, simplicity-first), trois jugements indépendants, puis **une passe de vérification des crates (33 entrées, crates.io/docs.rs du 2026-10-13) et une passe de réfutation (11 claims, deux tentatives indépendantes chacune, avec benchmarks reproduits sur Rust 1.97)**. Les corrections issues de ces deux passes sont intégrées ci-dessous et récapitulées en §14.

Ce document retient le découpage **simplicity-first** (gagnant des trois jugements), y greffe la couche campagnes et les garde-fous de perf-first, les règles de fill, `tv4 verify`, `tv4 bench`, `min_trades` et `forced_close` d'iteration-first, et tranche chaque objection soulevée. Critère de décision n° 1 inchangé : la performance du backtest et de l'ingestion, là où V3 a échoué.

Légende : **mesuré** = chiffre obtenu le 2026-10-13 sur un conteneur 4 vCPU Xeon 2,1 GHz / 15 Go / Rust 1.97 stable, build release, pendant l'étude ou la passe de réfutation ; **à valider** = estimation, donnée non vérifiée ou point à confirmer sur ton poste ou sur la doc officielle.

## 0. Les décisions, en une page

| Sujet | Décision | Pourquoi (une ligne) |
|---|---|---|
| Langage du moteur | **Rust 1.97 stable, 100 % du chemin bougie → signal → fill**, binaire `tv4` statique | Décision du propriétaire ; aucune frontière FFI par bougie (erreur du runtime Backtrader de V3) |
| Stratégies | **Types Rust derrière un trait `Strategy`**, un module (future crate) par stratégie, params en TOML | Scripting écarté sur mesures reproduites : PyO3 380-400 ns/bougie d'appel + 1-5 µs de corps Python, Rhai 3,4-4,1 µs avec état réel, wasmtime 22-38 ns mais à compiler quand même, natif 10-14 ns (mesuré) ; deux langages dans la boucle = la dérive multi-moteurs de V3 |
| Découpage | **Un seul paquet Cargo `tv4` (lib + bin)** dans un workspace, modules = futures crates, règle de dépendance écrite | Un débutant Rust seul ne doit pas payer 10 crates avant le premier trade ; scission mécanique quand un module dépasse 3 000 lignes ou quand l'incrémental dépasse 10 s (première candidate : `strategy`) |
| Stockage marché jalon 1 | **Parquet immuable par (exchange, symbole, TF, mois) + manifeste blake3**, pas de serveur de base | Le moteur lit Parquet de toute façon ; l'incrémental se fait par réécriture atomique du mois (30-60 ms mesurés avec fsync) ; DuckDB CLI (base embarquée, lecture seule) pour l'ad hoc |
| PostgreSQL / TimescaleDB | **Jalon 2**, pour les métadonnées (datasets, runs, lineage) via Symfony/Doctrine ; Timescale pour les bougies **seulement si** un besoin de requêtes temps réel apparaît au jalon 3 | Déviation explicite du défaut de 01-cadrage, actée le 13/10 (21:47), §13 |
| Orchestration jalon 1 | **cron + commandes idempotentes** (`tv4 ingest okx --resume`) | Temporal était déjà hors jalon 1 ; la reprise vient du design de la commande |
| Orchestration jalon 2 | Temporal reste le défaut. **Correction** : un SDK Rust applicatif officiel existe désormais (`temporalio-sdk` 1.1.0, Temporal, 2026-10-06, rust-version 1.92) ; le choix du worker (Rust natif, Python lanceur, ou Symfony Messenger) est tranché au jalon 2 sur le nombre de langages | La version précédente affirmait à tort qu'aucun SDK Rust supporté n'existait ; `temporal-sdk` (sans préfixe) n'existe pas, mais `temporalio-sdk` oui |
| Async | **Aucun au jalon 1** (HTTP synchrone `ureq`, 2-3 threads `std` pour l'ingestion OKX) ; tokio au jalon 3 avec le WebSocket | L'async est la partie où un débutant Rust perd le plus de temps et elle n'est pas sur le chemin du moteur ; la concurrence nécessaire à l'ingestion tient en threads std |
| Nombres | `f64` dans le moteur, arrondi tick/lot dans le simulateur ; `rust_decimal` à la frontière exchange au jalon 3 | Exact pour un backtest, inutile avant les ordres réels ; `rust_decimal` dans la boucle mesuré ×5 à ×300 |
| Python | **Zéro rôle dans le moteur**. Notebooks Polars/DuckDB sur les sorties CSV/Parquet, hors dépôt | Analyse des résultats seulement, sans couplage de code |
| Perf cible jalon 1 | 2 ans de BTC/USD 1m (dump Kraken XBTUSD) : **< 5 s mur-à-mur en release** (objectif officiel inchangé : < 60 s) ; mur-à-mur **mesuré 0,2-0,4 s** ; boucle mesurée à **6-18 ns/bougie mono-cœur** selon CPU et stratégie | 10 à 100× de marge, volontaire : on n'optimise rien de plus sans mesure criterion |
| Rapport du jalon 1 | **Squelette Symfony minimal** : commande console + templates Twig qui rendent le rapport HTML statique depuis `metrics.json`/`trades.csv`, sans base | Décision du propriétaire (05-ihm §7) ; les templates resservent à la fiche de run du jalon 2a |
| Outillage de recherche | **Jalon 1.5** : portefeuille multi-paires, profils de coûts nommés, contrôles aléatoires, bootstrap/Monte Carlo, registre d'essais, certification sur bruit (§15.1) | Exigences de 04-protocole §9 ; rien de tout cela n'est nécessaire à la tranche verticale du jalon 1 |
| Ordres limit multiples | **Une stratégie peut tenir plusieurs ordres limit en attente à la fois** (`Signal::PlaceLimit` achat ou vente partielle, identifié par `slot`, `CancelSlot`), cash et quantité **réservés à la pose**, plafond `max_pending` dans `config_hash` (§6, §7.2, §15.3) | Décision du propriétaire du 2026-10-13 (§15.3) : un grid avec filtre de tendance est une hypothèse de recherche à tester plus tard sur Bitvavo USDC ; rien de grid-spécifique dans le moteur, aucun grid au jalon 1 |
| Exploitation live (jalon 3) | **OKX spot seul, paires XXX-USDC (décision du 13/10 (22:46)), grille Regular 0,20/0,35 % définitive** (un avis OKX du 11/09/2026, en vigueur le 25/09/2026, annonce 0,10 % maker / 0,20 % taker pour le compte spot seul et 0,000 / 0,050 % sur les paires de stablecoins : le compte du propriétaire affiche 0,20 % le 14/10 (05:21), lu comme le maker de la grille 0,20 / 0,35 %, qui reste la baseline) ; `execution` sur une **machine dédiée à la maison**, sous `systemd` avec chien de garde ; ordres idempotents, stop algo OKX et stop logiciel de secours, réconciliation au démarrage puis à trois cadences, `capital alloué` par palier 60 → 150 → 300 € (§2) ; IHM par VPN seul (§15.2) | Décisions du propriétaire du 13/10 (18:35) : pas de compte dérivés (`okx_xperps` écarté), machine maison, pilote en échelle 20 → 50 → 100 % (option (b)), accès par VPN ; règles d'exécution reprises de 08 §C |

## 1. Principes

1. **Une seule implémentation de stratégie, trois contextes** : backtest, paper, live consomment le même code de stratégie et le même simulateur d'ordres (le live remplace le simulateur par le connecteur exchange). Inverse de V3 (replay Paper, runtime Backtrader et MTF live = trois moteurs). Conséquence nouvelle, imposée par Rust : les indicateurs sont écrits sous forme **incrémentale** (`update(x) -> f64`, O(1), ring buffer pour les fenêtres glissantes) et le helper batch est **défini comme le fold de `update`** (`bars.iter().map(|b| ind.update(b))`), jamais comme une implémentation vectorisée séparée : sinon backtest et paper divergent sur les flottants (mesuré : SMA à somme glissante vs SMA recalculée diffèrent de 1,2e-9 sur 99 % des points). Les colonnes de bougies sont des buffers croissants (`reserve` à la taille du dataset en backtest, bornés par `max_bars` en live). Mesuré : l'incrémental coûte *moins* que le précalcul batch (65 ms vs 114 ms pour 2 ans, deuxième passe mémoire évitée). Invariants associés, dès le squelette : horloge = timestamp des événements (jamais `Instant::now()`/`SystemTime` dans `engine`, `sim`, `strategy`) ; trait `Strategy` et boucle synchrones (pas d'`async fn on_bar`) ; entrée du simulateur typée `MarketEvent::ClosedBar` dès le jalon 1, avec les règles de fill exprimées comme « le chemin de prix a traversé le niveau », pour que `Trade`/`BookTop` puissent s'ajouter au jalon 3 sans second modèle. Le modèle de fill du paper (bougie clôturée vs trades) reste à trancher à la conception du mode paper.
2. **Pas de look-ahead par bornage d'index, sous invariants testés** : une stratégie ne voit que des tranches `[..closed_count]` de bougies clôturées, sur tous ses timeframes. **Correction (réfutation)** : ce bornage interdit la lecture d'index futurs, il ne garantit ni la causalité des colonnes exposées (un indicateur précalculé non causal, z-score plein échantillon ou moyenne centrée, fuit *à travers* la tranche) ni la justesse de `closed_count` (un off-by-one de frontière expose la bougie TF en cours). La garantie vient de quatre invariants, chacun avec son test (§7.5), et d'une convention encapsulée (champs privés, accesseurs bornés, `#![forbid(unsafe_code)]`). Le modèle de menace est le bug accidentel, pas l'adversaire.
3. **Reproductibilité** : chaque run est identifié par `config_hash` (stratégie, version, params, frais, slippage, règles du simulateur, version du moteur) et `dataset_hash` (contenu logique du dataset figé, encodage canonique `dsh-v1` possédé par le projet). Deux runs à hashes identiques donnent des fichiers identiques à l'octet ; la CI le vérifie. Conditions d'ordre des opérations explicitées en §7.4.
4. **Coûts nets dès le premier backtest** : frais maker/taker par exchange (OKX 0,20/0,35 %, Bitvavo USDC 0,05/0,05 %), slippage, et rejet par défaut de toute config à expectancy nette ≤ 0 **ou** à moins de `min_trades` trades (défaut 30, exigence de 01-cadrage).
5. **Petit périmètre, CI complète, zéro dette au départ** : `clippy -D warnings`, `rustfmt --check`, tests, couverture, sur 100 % du code dès le squelette.
6. **Un seul langage dans le moteur** : Rust, sans scripting, sans FFI par bougie, sans interpréteur embarqué. Ajouté par cette révision.
7. **Le plus petit nombre de pièces mobiles au jalon 1** : ce qui n'est pas « lire des bougies, appliquer une stratégie, simuler des ordres, écrire des métriques » est reporté avec son critère de retour écrit (§12). Ajouté par cette révision.
8. **Discipline de SDK sans allocation ni hachage par bougie** : indicateurs exposés par tranches typées indexées (jamais par `HashMap<String, _>`), signaux dans un `Vec` réutilisé, aucune copie de fenêtre de bougies dans le contexte, aucun recalcul d'indicateur dans `on_bar`. Mesuré : c'est cette discipline, et non des optimisations bas niveau, qui sépare 10 ns/bougie de 100-200 ns/bougie. Ajouté par la passe de réfutation.

## 2. Modules et flux

```
            ┌──────────────────────────────────────────────────────┐
            │                  CONTROL PLANE (jalon 2)             │
            │   Symfony 7 + API Platform : catalogue stratégies,   │
            │   configs versionnées, runs, résultats, lineage      │
            │   (PostgreSQL ; tables = projection des manifestes)  │
            └───────────────┬──────────────────────┬───────────────┘
                            │ lance `tv4 …`        │ lit runs/*/manifest.json
                            │ (Temporal Rust/py    │ (import Doctrine)
                            │  ou Messenger, j. 2) │
                            ▼                      ▼
  ┌─────────────┐   ┌───────────────┐   ┌───────────────────────┐   ┌──────────────┐
  │ marketdata  │──▶│ store         │──▶│ engine                │──▶│ report       │
  │ Kraken zip  │   │ Parquet       │   │ mtf (1m → TF, index)  │   │ trades.csv   │
  │ OKX REST    │   │ mensuels +    │   │ indicators (Vec<f64>) │   │ metrics.json │
  │ (ureq, sync,│   │ manifestes    │   │ strategy (trait)      │   │ equity.csv   │
  │  3 threads) │   │ dataset_hash  │   │ sim (ordres, frais,   │   │ manifest.json│
  │ continuity  │   └───────────────┘   │   slippage, spot)     │   └──────────────┘
  └─────────────┘                       └───────────┬───────────┘
         ▲                                          │
         │ cron : tv4 ingest okx --resume           │ même strategy + sim, flux WS
         │                                          ▼
  ┌──────┴──────┐                       ┌───────────────────────┐
  │ research    │ jalon 1.5 / 2         │ execution (jalon 3)   │
  │ grid, wfo   │ rayon, Arc<Dataset>   │ paper : WS public     │
  └─────────────┘                       │ live : OKX + gates    │
                                        └───────────────────────┘
```

Tous les modules du jalon 1 vivent dans **un seul paquet Cargo `tv4`** ; chaque module est une future crate. Règle de dépendance, tenue à la main et vérifiée par un test CI de `grep` sur les `use` (clippy ne la vérifie pas) : `model` ne dépend de rien ; `strategy` ne dépend que de `model` ; `sim` ne dépend pas de `strategy` ; `indicators` ne dépend que de `model` ; `engine` orchestre tout ; `cli` n'a aucune logique métier.

| Module (`tv4/src/…`) | Responsabilité | Entrées / sorties | Jalon |
|---|---|---|---|
| `model` | Types partagés, `Copy` quand possible : `Candle`, `Timeframe`, `FeeSchedule`, `Slippage`, `Side`, `MarketEvent`, erreurs (`thiserror`) ; JSON canonique + blake3 (code applicatif : `blake3` ne fournit que `hash`/`Hasher`) | — | 1 |
| `marketdata` | Lecteur des dumps Kraken OHLCVT (zip → csv en flux, SHA-256 vérifié), client REST OKX `history-candles` (100/req, pagination `after` vers le passé, **2-3 threads std + token bucket partagé à 8 req/s**, curseur dérivé du fichier, reprise, retry sur HTTP 429/5xx **et** code métier `50011`), rapport de continuité (trous, doublons, bougie `confirm = 0`, OHLC incohérent) | Fichiers / API → bougies normalisées | 1 (Kraken, OKX REST) ; WS en 3 |
| `store` | Parquet immuables par (exchange, symbole, TF, mois), écriture `.tmp` + `sync_all` + `rename` + fsync du répertoire, `_manifest.json` par fichier, construction de dataset figé (manifeste + `dataset_hash`), lecture en colonnes SoA, un seul écrivain par partition | Bougies → Parquet → `Vec<f64>` | 1 ; `PgSink` (sqlx) en 2 |
| `mtf` | Agrégation 1m → TF supérieurs (frontières UTC) + index `closed_count[tf][i]` défini par `close_time(bar_tf) ≤ close_time(bar_1m_i)` ; les fichiers Kraken 5m/1h servent d'oracle de test de l'agrégation, jamais de source | 1m → séries TF + index | 1 |
| `indicators` | SMA, EMA, RSI, ATR, VWAP incrémentaux (`update`) + batch = fold de `update`, NaN en warmup ; test de stabilité par préfixe ; cache mémoire par (TF, spec) pendant une campagne, précalculé avant la grille, plafond mémoire explicite | Séries → colonnes | 1 |
| `strategy` | Trait `Strategy`, `BarContext`, `Signal`, `IndicatorSpec`, registre à `match` explicite, params serde ; `sma_cross` de référence ; `#![forbid(unsafe_code)]` | Contexte → signaux | 1 |
| `sim` | Portefeuille spot (pas de short, pas de levier, `cash ≥ 0`), ordres market/limit(TTL)/stop/take-profit, **plusieurs limits en attente par stratégie** (achat ou vente partielle, cash et quantité réservés à la pose, plafond `max_pending`), `IntrabarRule`, frais par exchange, slippage, sizing par risque, arrondi tick/lot, journal des trades FIFO par lot avec MFE/MAE | Signaux + événements → fills, positions | 1 |
| `engine` | Préparation (warmup, MTF, indicateurs), boucle 1m, `config_hash`, `RunManifest` ; fonction pure `run_cell(&Dataset, &IndicatorCache, &CellConfig) -> CellResult` | Dataset + config → run | 1 |
| `report` | `metrics.json` (verdict inclus), `trades.csv`, `equity.csv` échantillonné, `manifest.json` avec `result_hash` ; seul module autorisé à appeler `ln`/`exp`/`powf` | Run → fichiers | 1 |
| `cli` | `clap` : `ingest`, `dataset build`, `backtest`, `verify`, `bench`, `grid` | — | 1 (`grid` en 1.5) |
| `research` | Grilles et walk-forward : `Arc<Dataset>` chargé une fois, cache d'indicateurs immuable partagé, `rayon` une cellule par tâche, résultats réduits dans le worker et collectés dans l'ordre de la grille, `WfoPlan` hashé, holdout exclu par construction | Engine en boucle | 1.5 (grid), 2 (wfo) |
| `execution` | Paper : simulateur branché sur le WS public de l'EEE (`wseea.okx.com` ; filtre `confirm = 1`, dédup, backfill REST). Live : connecteur OKX spot seul sur `eea.okx.com` (WebSocket `wseea.okx.com`, démo `wseeapap.okx.com`), paires XXX-USDC, derrière les gates V3 (kill switch, pas d'entrée sans stop, plafond de notional, flag explicite), plus les règles d'ordres, de stop, de réconciliation, de service, de secrets et de capital écrites sous ce tableau | WS/REST ↔ ordres | 3 |
| `control-plane/` (jalon 1) | Squelette Symfony : commande `tv4:report` + Twig, rend le rapport HTML statique d'un run (sections de la fiche de run de 05-ihm, Distribution comprise) | Fichiers du run → HTML | 1 |
| `control-plane/` (jalon 2-3) | Symfony 7 + API Platform : stratégies, versions, manifestes, runs, lineage, registre d'essais ; importe les `manifest.json` ; déclenche les campagnes ; commandes de risque et flux au jalon 3 (§15.2) | HTTP ↔ PostgreSQL | 2-3 |

**Module `execution` (jalon 3) : règles fixées avant le code** (08 §C, décisions du 13/10 (18:35)). Rien de ceci n'entre au jalon 1 (§11).

- **Ordres idempotents.** `clOrdId` et `algoClOrdId` déterministes = préfixe + hash de (bot, paire, horodatage de la bougie de décision, intention, slot), 32 caractères (à valider : format accepté par OKX et rejet d'un doublon, code 51016). L'intention est écrite en base **avant** l'envoi, avec un état « envoyé / ambigu ». Un timeout ou un 5xx n'est jamais réémis sans un `GET /trade/order?clOrdId` qui tranche : **aucun retry automatique sur un POST d'ordre** (§3). Une intention d'entrée expire à la clôture suivante du TF de décision ; une intention de sortie ou de stop est poursuivie jusqu'à un état confirmé. `cancel-all-after` n'est jamais activé (à valider que les algos n'y sont pas soumis). Chaque ordre vise une paire XXX-USDC du carnet USD unifié (décision du 13/10 (22:46) ; les carnets XXX-EUR, 2 à 6 fois plus minces, sont écartés) et passe `tradeQuoteCcy = USDC` après lecture de `tradeQuoteCcyList` sur l'instrument, avec `tdMode = cash` : OKX Europe propose la marge spot jusqu'à 10×, le moteur n'active jamais l'emprunt.
- **Stop.** Ordre algo OKX déclencheur → market (`orderPx = −1`), déclencheur sur le dernier prix, taille = solde base après frais arrondi au `lotSz`, posé < 60 s après le fill. Si l'algo manque ou est refusé : réaction graduée de 08 §C.3.1 (reposer l'algo avec retry et backoff, sinon sortie limit à prix borné), jamais un market aveugle. Tant qu'aucun algo n'est confirmé, **stop logiciel de secours** : le moteur surveille la 1m et envoie la sortie limit bornée dès que le niveau de stop est franchi, reposée à chaque 1m tant qu'elle n'est pas remplie ; l'état reste critique jusqu'à un algo confirmé.
- **Réconciliation.** Au démarrage et après toute coupure réseau ou API > 1 min : état reconstruit depuis PostgreSQL, puis, dans cet ordre, adopter les positions par `clOrdId`, rattacher les algos par `algoClOrdId`, annuler les orphelins de notre schéma de nommage (avertissement résolu automatiquement). Un ordre, un algo ou une position hors schéma est critique : PAUSE, décision humaine ; une position hors schéma n'est jamais vendue. Aucune entrée avant une réconciliation propre. En marche, trois cadences : existence et taille de l'algo stop de chaque position toutes les 30 à 60 s ; ordres ouverts et positions toutes les 5 min ; fills et soldes toutes les heures, avec une réconciliation complète chaque jour (budget < 1 requête/s).
- **Service.** `systemd` avec `Type=notify`, `WatchdogSec=60`, `Restart=always` et un `sd_notify` à chaque cycle : un processus bloqué sans crasher est tué et relancé. Battement écrit en base à chaque cycle ; « sans battement > 5 min » = alerte critique. `ExecStartPre` refuse de démarrer si `var/kill` existe.
- **Secrets.** Clé OKX dans un fichier `sops`/`age` déchiffré en mémoire au démarrage, ou passée par `systemd LoadCredentialEncrypted=` ; jamais dans une variable d'environnement `docker compose` (lisible par `docker inspect` et `/proc`). Rien dans git. La clé est créée sur le site EEE et ne sert que sur ses domaines (`eea.okx.com`, `wseea.okx.com`) : sur un autre domaine, elle renvoie l'erreur 50119. Elle est liée à l'IP de la machine (20 IP au plus par clé) ; une clé de trading non liée à une IP est supprimée après 14 jours d'inactivité.
- **Capital.** Paramètre `capital alloué` du connecteur, lu au démarrage, jamais une constante : 60 € au palier 20 % (G6), 150 € au palier 50 %, 300 € au palier 100 % (G7), en USDC après conversion au passage de palier (décision du 13/10 (18:35), option (b) ; USDC et non USDT, décision du 13/10 (22:46)), les euros étant convertis par la paire USDC-EUR (frais du groupe stablecoins 0,000 / 0,050 % selon l'avis OKX du 11/09/2026, à valider). Les plafonds en dérivent : notional cumulé ≤ capital alloué, par ordre ≤ risque / stop minimal admis, 5 % de cash en marge sur tout market buy. Seuils en USDC sur le capital alloué du palier en cours ; 1 R = 1 % de l'equity courante du palier (mode capitalisé, 04 §9.6).
- **Machine.** Paper et live tournent sur une machine dédiée à la maison (par ordre de préférence : une machine déjà possédée, un mini-PC x86 reconditionné, ou un Raspberry Pi 5 d'au moins 4 Go avec SSD NVMe, alimentation 27 W, refroidissement actif et pile RTC ; jamais PostgreSQL sur carte SD ; étude du 13/10 dans `notes/raspberry-pi-machine-dediee.md`), jamais sur le poste de développement ; horloge synchronisée avant démarrage (`systemd-time-wait-sync`, refus de démarrer sinon) ; aucune campagne ni `cargo build` dessus. VPS seulement au-delà de ≈ 2 000 € de capital ou si la connexion coupe plus de 15 min plus d'une fois par mois pendant le paper (mesuré dans le Journal). Build aarch64 au jalon 3 seulement, si le Pi 5 est retenu ; les jalons 1 et 2 restent en `x86-64-v3` (§4) ; le rejeu hebdomadaire (§15.1 ligne 14) compare alors un binaire x86-64 à un binaire aarch64 : identité des signaux à valider (§7.4) ; si le Pi 5 est retenu, le golden test de CI tourne aussi en aarch64 (runner arm64 ou QEMU) avant G5, condition du choix. IHM joignable par VPN seul (§15.2).

## 3. Stack par module, pourquoi, alternative écartée

| Couche | Choix | Pourquoi | Alternative écartée |
|---|---|---|---|
| **Moteur** (`marketdata`, `store`, `mtf`, `indicators`, `strategy`, `sim`, `engine`, `report`, `cli`) | **Rust 1.97 stable**, un paquet `tv4`, colonnes SoA `Vec<f64>`, `Box<dyn Strategy>`, `BarContext<'a>` à tranches bornées, `Vec<Signal>` réutilisé | Boucle complète mesurée et **reproduite indépendamment deux fois** à **10-14 ns/bougie** (trait object, 2 TF, limit/stop/TP, MFE/MAE, equity ; 6-18 ns selon CPU, stratégie et nombre de trades) soit 12-20 ms pour 2 ans de 1m, mono-cœur ; binaire sans runtime ; le compilateur remplace une partie du debug ; un seul langage jusqu'au live | Python/Polars (version précédente) : 1 à 5 µs par bougie en boucle, suffisant au jalon 1 mais ×20-50 plus lent sur grilles/WFO et un langage de plus. PHP : V3 l'a prouvé. Stratégies scriptées (PyO3, Rhai, wasm) : voir §3.2 et §9 |
| **Données colonnaires** | **arrow + parquet 60.0.0** (zstd), `RecordBatch` à 7 colonnes, lecture par `ParquetRecordBatchReaderBuilder` avec `with_projection` et `with_row_groups` (filtre **manuel** sur les statistiques des row groups, la crate ne l'automatise pas) | Format standard lisible par DuckDB/Polars en notebook ; lecture 2 ans × 6 colonnes **60-150 ms mesurés à chaud selon le codec** (brut 59-88, snappy 83-145, zstd 115-146), ~350 ms à froid | polars (Rust, 0.55.2) : compile lourde (351 crates, 2,9 Go de `target`, 126 s avant une erreur d'unification de features reproduite), API mouvante, inutile pour une boucle en SoA. Crate duckdb (1.10506.0) : compile C++ longue avec `bundled` ; on utilise le binaire DuckDB CLI |
| **Stockage marché** | **Parquet seul au jalon 1**, un fichier par mois = unité de réécriture, un row group par fichier (≈ 44 640 lignes via `set_max_row_group_row_count`), `set_created_by("tv4")` | Le moteur ne lit jamais une base ; l'incrémental OKX réécrit le mois courant (43 200 lignes, **30-60 ms mesurés** avec fsync) atomiquement ; `dataset_hash` sur le contenu ; 150 lectures DuckDB pendant 400 remplacements par rename : 0 erreur (46 erreurs en réécriture sur place) | TimescaleDB au jalon 1 : une base à opérer, `sqlx` async, sans lecteur. Revient au jalon 2 pour les métadonnées, jalon 3 pour les bougies si besoin temps réel. ClickHouse/QuestDB : une base de plus |
| **HTTP** | **ureq 3.4** synchrone, **2-3 threads `std::thread::scope` partageant un `Agent` cloné et un token bucket maison** (`Mutex<Instant>`, 125 ms d'espacement, global par IP, comptant les retries), retry exponentiel avec jitter (250 ms → 8 s, 5 essais) sur 429/5xx/timeout et sur JSON `code != "0"` (`50011`), pause de 2 s de tout le bucket après un 429 ; ce retry sert aux lectures de `marketdata` et **jamais** à un POST d'ordre (`execution`, §2 : un timeout ou un 5xx s'y tranche par `GET` sur `clOrdId`) | Zéro async au jalon 1. **Correction (réfutation)** : un seul fil est borné par le RTT France → Asie (≈ 250 ms, TTFB médian 245-257 ms mesuré depuis Londres/Francfort par ccxt) : 2,65 req/s avec `sleep` après réponse (≈ 33 min pour 1 an), 4 req/s en cadence fixe (≈ 22 min) ; 3 threads + bucket mesurés à 7,95 req/s (≈ 11 min). `ureq` ne fournit ni rate limit ni retry | reqwest 0.13 + tokio + governor 0.10 : corrects mais c'est l'apprentissage le plus coûteux pour un débutant, et pas sur le chemin critique ; `governor` fonctionne aussi en synchrone, option si le bucket maison déplaît. `backon` (1.6) : optionnel |
| **Orchestration** | Jalon 1 : **cron + commande idempotente**. Jalon 2 : Temporal (worker **Rust `temporalio-sdk` 1.1.0** ou Python lanceur) **ou** Symfony Messenger | Reprise et retries viennent de la commande ; Temporal garde sa valeur (visibilité, schedules) au jalon 2-3 | **Correction** : `temporal-sdk` n'existe pas sur crates.io, mais `temporalio-sdk` (repo temporalio/sdk-rust, 1.0.0 le 2026-09-04, 1.1.0 le 2026-10-06, macros `#[workflow]`/`#[activity]`, Worker, Runtime, replayer) est un SDK applicatif officiel ; l'ancienne `temporal-sdk-core` 0.1.0-alpha.1 (2021) est obsolète |
| **Control plane** | **Symfony 7.3 / PHP 8.4, API Platform, Doctrine** (jalon 2) ; les tables `datasets`/`runs` sont une projection des `manifest.json` produits par `tv4` | Ton cœur de compétence ; CRUD métier, pas de calcul ; aucune double source de vérité | FastAPI : plus simple à court terme, mais perd Symfony là où il compte |
| **Cache / temps réel** | Redis, jalon 3 seulement | État paper/live, dernier prix | Rien au jalon 1 |
| **Front** | Jalon 1 : squelette Symfony minimal (commande console + Twig) rendant le rapport HTML statique depuis les fichiers du run ; jalon 2a : pages Recherche et Stratégies (05-ihm) ; DuckDB CLI pour l'ad hoc | Décision du propriétaire du 2026-10-13 ; aucune base au jalon 1 | Rapport jetable côté moteur (templating Rust) : à réécrire au jalon 2 |
| **Qualité** | `rustfmt`, `clippy -D warnings` (+ `clippy::unwrap_used` en deny dans la lib, `disallowed_types` sur `HashMap`/`HashSet` et `disallowed_methods` sur `ParallelIterator::sum/reduce` dans le chemin de calcul), tests `#[test]`, `proptest`, `insta`, `criterion`, `cargo-llvm-cov` ; CI GitHub Actions sans filtrage par chemin | V3 : 1 832 erreurs PHPStan, CI partielle | `clippy::pedantic` global : trop de bruit au départ ; activer lint par lint. `clippy::suboptimal_flops` (pousse vers `mul_add`) : désactivé, voir §7.4 |

### 3.1 Crates retenues (versions crates.io vérifiées le 2026-10-13 par `cargo info`, docs.rs et l'API crates.io ; à figer dans `Cargo.lock`)

| Crate | Version vérifiée | Rôle | Jalon | Note de vérification |
|---|---|---|---|---|
| `clap` (derive) | 4.6.7 (2026-09-14) | CLI `tv4` | 1 | `#[derive(Parser, Subcommand)]` ; rust-version 1.85 |
| `serde`, `serde_json` | 1.0.229 / 1.0.154 | Params, manifestes, metrics, JSON à clés triées pour les hashes | 1 | Clés triées **uniquement** via `serde_json::to_value(&s)` puis sérialisation de la `Value` (BTreeMap), jamais `to_string(&struct)` (ordre de déclaration) ; interdire la feature `preserve_order` (vérifier `Cargo.lock`) ; ce n'est pas RFC 8785/JCS, suffisant pour un hash interne, `serde_jcs` à vérifier si une canonicalisation stricte devient nécessaire |
| `toml` | 1.1.8 (2026-10-09) | `config/fees.toml`, params de stratégie | 1 | `toml::from_str` et `Value::try_into::<T>()` vérifiés par compilation |
| `arrow`, `parquet` | =60.0.0 (2026-09-15, épinglé strict) | Écriture/lecture Parquet, zstd | 1 | `set_max_row_group_row_count(Option<usize>)` et `set_max_row_group_bytes` confirmés (l'ancien `set_max_row_group_size` n'existe plus) ; `Some(0)` panique ; `arrow` et `parquet` doivent partager la même majeure ; rust-version 1.88 ; `zstd` dans les features par défaut |
| `csv` | 1.4.0 (2025-10-17) | Dumps Kraken sans en-tête ; `trades.csv`, `equity.csv` | 1 | `ReaderBuilder::has_headers(false)`, lecture en flux ; API gelée, peu de releases = maturité |
| `zip` | 9.0.2 (2026-10-11) | Lecture des `Kraken_OHLCVT_*.zip` sans extraction | 1 | `ZipArchive::by_name` + `impl Read for ZipFile` vérifiés dans la source ; `default-features = false, features = ["deflate"]` (les défauts 9.x tirent zstd, lzma, ppmd, xz) |
| `sha2` | **0.11.1** (2026-10-12) | Vérification des SHA-256 publiés par Kraken | 1 | **Corrigé** : 0.10 → 0.11 (digest 0.11, MSRV 1.88) ; 0.10.9 reste légitime si une dépendance impose digest 0.10 |
| `ureq` | 3.4.2 (2026-09-13) | Client REST OKX synchrone | 1 | `Agent` clonable avec pool partagé ; `http_status_as_error(false)` pour lire le corps d'erreur OKX ; feature `json` |
| `chrono` | 0.4.45 (2026-06-04) | Dates aux bords (CLI, affichage) ; en interne `i64` ms UTC | 1 | `from_timestamp_millis` / `timestamp_millis` `const fn` ; test CI de `grep` : `chrono` absent de `engine`/`sim`/`strategy` |
| `blake3` | 1.8.7 (2026-08-20) | `dataset_hash`, `config_hash`, `result_hash` (code applicatif sur `blake3::Hasher`) | 1 | La crate ne hache que des octets ; la canonicalisation est à nous (§5.3) |
| `thiserror`, `anyhow` | 2.0.21 / 1.0.104 | Erreurs typées dans la lib, `anyhow` dans le binaire | 1 | — |
| `tracing`, `tracing-subscriber` | 0.1.44 / 0.3.23 | Logs structurés, spans par phase (load, indicators, loop, report) | 1 | Feature `json` de `tracing-subscriber` pour la sortie structurée ; rester sur 0.1/0.3 (0.2 non publié) |
| `indicatif` | 0.18.6 (2026-07-01) | Barre de progression de l'ingestion OKX | 1 (optionnel) | `ProgressBar::hidden()` pour rendre l'affichage optionnel |
| `rayon` | 1.12.0 (2026-04-14) | `par_iter` sur les cellules d'une grille, données en `Arc` | 1.5 | Exige `Fn + Sync + Send` sur la fermeture et `Send` sur le résultat ; voir §7.6 |
| `criterion` (dev) | 0.8.2 (2026-02-04) | Bench `engine_2y_1m`, ns/bougie archivé | 1 | `Throughput::Elements` pour le coût par bougie ; baselines `--save-baseline` ; l'archivage CI lit `target/criterion/*/estimates.json` (code projet) |
| `proptest` (dev) | 1.12.0 (2026-10-10) | Indicateurs vs implémentation naïve, agrégation MTF, anti-look-ahead par perturbation | 1 | `cases ≥ 1 000` pour le test de perturbation |
| `insta` (dev) | 1.49.0 (2026-10-03) | Snapshots de `metrics.json` et `trades.csv` sur fixture | 1 | Lire le fichier puis `assert_snapshot!`, ou `assert_json_snapshot!`/`assert_csv_snapshot!` (features `json`/`csv`) ; `cargo-insta` pour accepter |
| `tempfile` (dev) | 3.27.0 (2026-03-11) | Répertoires temporaires pour les tests d'écriture Parquet | 1 | — |
| `sqlx` (postgres) | 0.9.0 (2026-05-21) | `PgSink` derrière une feature `pg-sink` du projet | 2 | `copy_in_raw`/`PgPoolCopyExt` confirmés ; **exige Rust ≥ 1.94** ; features `postgres` + `runtime-tokio` + `tls-*` séparées |
| `rust_decimal` | 1.43.0 (2026-09-02) | Quantités/prix à la frontière exchange | 3 | `round_dp_with_strategy`, `dec!` ; jamais dans la boucle (mesuré 47-70 ns/bougie en passif, 5 µs avec arithmétique) |
| `tokio`, `tokio-tungstenite` | 1.53.2 / 0.30.0 | WebSocket OKX, paper | 3 | `connect_async_tls_with_config` ; **aucun backend TLS par défaut** : activer `rustls-tls-webpki-roots` pour `wss://` |

### 3.2 Écartées, avec la raison

| Crate | Verdict |
|---|---|
| `polars` 0.55.2 (Rust) | Écartée : compile lourde (351 crates, 2,9 Go de `target`), API mouvante (0.x), erreur d'unification de features reproduite pendant l'étude, aucun gain pour une boucle SoA. Polars Python en notebook lit les mêmes Parquet |
| `duckdb` 1.10506.0 (crate) | Écartée : la feature `bundled` (requise pour `parquet`) compile l'amalgamation C++ ; le binaire DuckDB CLI (1.5.6 testé) suffit pour l'ad hoc |
| `pyo3` 0.29.3 + `maturin` 1.15.0 | Écartées au jalon 1 : 380-400 ns/bougie d'appel mesurés (fourchette 131-494 confirmée) + 1-5 µs de corps Python, soit ×150-500 vs natif ; 4 cellules sous rayon mesurées ×4,4 plus lentes qu'une seule (GIL, CPython 3.13 standard) ; libpython au runtime. Nuance : Python 3.14 free-threaded (PEP 779) est supporté par PyO3 ≥ 0.23 et un process par cellule contourne le GIL, donc l'argument décisif est le coût par bougie sur les grilles (2 ans × 1 000 cellules ≈ 7 min PyO3 vs 12 s Rust sur 4 cœurs), pas le GIL. Option future pour exposer `backtest()` à un notebook |
| `rhai` 1.26.1 | Écartée : 915-1 147 ns/bougie sur une fonction triviale, **3,4-4,1 µs mesurés avec un état réel** (`Map` + `Array`, feature `sync`), ×300 vs natif ; interpréteur 2-3× plus lent que CPython selon ses propres benchmarks ; aucun écosystème numérique |
| `wasmtime` 49.0.2 | Écartée : 22-38 ns/bougie mesurés (claim de 21 ns confirmé à l'ordre de grandeur) mais la stratégie reste à compiler (guest `no_std` wasm32, bindings à écrire, marshalling du contexte) ; le seul gain d'itération (recompiler le guest en 0,5 s sans relinker l'hôte) ne vaut pas ce coût. Option future pour des stratégies tierces sandboxées |
| `libloading` 0.9.0 / `abi_stable` 0.11.3 | Écartées : ABI Rust instable (libloading = dlopen seulement), `abi_stable` sans release depuis 2023-10 et sans rechargement à chaud sûr ; aucun gain sur un module du workspace |
| `ta` 0.5.0, `yata` 0.7.0 | Écartées : `ta` non publiée depuis juin 2021, `yata` depuis mars 2024 ; un indicateur maison = 50 lignes, warmup, NaN et stabilité par préfixe maîtrisés. `yata::Method::next/over` est le modèle du pattern `update` + batch retenu |
| `reqwest` 0.13.5 / `governor` 0.10.4 / `backon` 1.6.0 | Différées : existent, reviendront avec tokio au jalon 3 si le connecteur en a besoin ; `governor` est une alternative synchrone au token bucket maison |
| `rusqlite` 0.40.2 | Différée : candidate au jalon 2 si le catalogue JSON des runs devient pénible avant Postgres ; `features = ["bundled"]` (les défauts incluent une cible wasm) |
| `smallvec` 1.16.3, `mimalloc` 0.1.52, `enum_dispatch` 0.3.13 | Écartées : gain inférieur au bruit de mesure face à 10-14 ns/bougie (mimalloc mesuré : aucun gain ; monomorphisation : ≤ 2 ns) ; complexité sans contrepartie |
| `temporal-sdk` | **N'existe pas sous ce nom** ; la crate officielle est `temporalio-sdk` 1.1.0 (§3 Orchestration), différée au jalon 2 |

### 3.3 Outillage cargo (pas des dépendances)

Tous vérifiés sur crates.io le 2026-10-13, compatibles Rust 1.97 : `cargo-llvm-cov` 0.9.1 (couverture, workflow GitHub Actions fourni dans son README), **`flamegraph` 0.6.14** (le nom de la crate est `flamegraph`, pas `cargo-flamegraph` ; `cargo install flamegraph` donne `cargo flamegraph` ; nécessite `perf` sur Linux), `cargo-nextest` 0.9.148 (confort, pas une nécessité), `cargo-deny` 0.20.2 (jalon 2, audit licences/advisories), `sccache` 0.18.0 et `cargo-chef` 0.1.78 (existent ; leur gain sur ton poste reste à mesurer). DuckDB CLI pour les requêtes ad hoc sur Parquet et CSV (`read_parquet(..., hive_partitioning=true)`, `parquet_metadata()`, `parquet_kv_metadata()` testés).

## 4. Arborescence du dépôt

```
tradingV4/
├── Cargo.toml                  # [workspace] members = ["tv4"], resolver = "2"
│                               # [workspace.dependencies] versions épinglées
│                               # [profile.dev] opt-level = 1, debug = "line-tables-only"
│                               # [profile.dev.package."*"] opt-level = 3   (dépendances optimisées en dev :
│                               #     mesuré, le décodage Parquet non optimisé domine le run debug)
│                               # [profile.release] lto = "fat", codegen-units = 1, debug = 1 (flamegraph),
│                               #                   overflow-checks = true  (jalon 1 : sécurité > 2 % de perf)
├── .cargo/config.toml          # [build] rustflags = ["-C", "target-cpu=x86-64-v3"]  (fixe, jamais `native` :
│                               #     « même binaire » doit être vrai entre CI et poste)
├── rust-toolchain.toml         # channel = "1.97.0"
├── rustfmt.toml, clippy.toml   # clippy.toml : disallowed-types = HashMap/HashSet dans engine/sim/indicators,
│                               #               disallowed-methods = rayon ParallelIterator::sum/reduce
├── .github/workflows/ci.yml    # fmt --check, clippy -D warnings, test, llvm-cov, backtest de fumée ×2 + verify,
│                               # golden dataset_hash, bench informatif ; cache cargo (Swatinem/rust-cache)
├── config/
│   ├── fees.toml               # [okx] maker_bps = 20 taker_bps = 35 ; [bitvavo_usdc] maker_bps = 5 taker_bps = 5
│   └── strategies/sma_cross.toml
├── docs/dataset-hash.md        # spécification de l'encodage canonique dsh-v1 (recalculable en PHP au jalon 2)
├── data/                       # gitignoré ; Parquet immuables
│   └── candles/kraken/BTC-USD/1m/2024-01.parquet …    (XBTUSD Kraken → BTC-USD canonique : XBTUSD est coté depuis le 06/10/2013, XBTUSDT seulement depuis le 20/12/2019, trop tard pour DEV 2018-2022 ; l'USD est aussi la meilleure approximation de l'USDC du lieu d'exécution)
│       candles/kraken/BTC-USD/1h/2024.parquet          (TF ≥ 1h : un fichier par an, sinon fichiers minuscules)
├── state/                      # verrous par partition ; curseur OKX optionnel (redondant avec max(open_time))
├── datasets/                   # manifestes JSON de datasets figés (liste de fichiers, fenêtre, gaps_policy, dataset_hash)
│   └── snapshots/<dataset_hash>/   # copie figée des mois encore mutables au moment du build
├── runs/<run_id>/              # manifest.json, metrics.json, trades.csv, equity.csv, progress.json
├── research/                   # H-NNN.md (fiches gelées, tag git frozen/H-NNN), trials.csv (append-only), journal.md, README.md (compteurs) — 04 §4
├── fixtures/                   # 10 000 bougies Kraken réelles (CSV) + Parquet golden + snapshots insta + dataset_hash attendu
├── notebooks/                  # gitignoré ; analyse des sorties, jamais importé par le moteur
├── control-plane/              # jalon 1 : squelette Symfony minimal (commande tv4:report + Twig) ; jalon 2 : API Platform + Doctrine
├── docker/Dockerfile           # multi-stage (cargo-chef 0.1.78), pas de compose au jalon 1
└── tv4/
    ├── Cargo.toml              # [lib] + [[bin]] name = "tv4"
    ├── build.rs                # git sha → env!("TV4_GIT_SHA")
    ├── src/
    │   ├── main.rs, lib.rs
    │   ├── cli/                # une fonction par sous-commande, aucune logique métier
    │   ├── model.rs            # Candle, Timeframe, FeeSchedule, Slippage, Side, MarketEvent, erreurs, hash.rs (JSON trié + blake3)
    │   ├── marketdata/         # kraken.rs, okx.rs (threads + bucket), continuity.rs
    │   ├── store/              # parquet.rs (write_month, read_range), dataset.rs (manifeste, dataset_hash, dsh-v1), columns.rs (SoA), lock.rs
    │   ├── mtf.rs              # agrégation 1m → TF, closed_count (par timestamps), just_closed
    │   ├── indicators/         # sma.rs, ema.rs, rsi.rs, atr.rs, vwap.rs : struct incrémentale + fn batch = fold(update)
    │   ├── strategy/           # mod.rs (trait, BarContext, Signal), registry.rs, sma_cross.rs ; #![forbid(unsafe_code)]
    │   ├── sim/                # portfolio.rs, orders.rs, fills.rs, fees.rs, sizing.rs
    │   ├── engine/             # run.rs (boucle, run_cell pur), config.rs (config_hash), manifest.rs
    │   ├── report/             # metrics.rs, writers.rs, verdict.rs (seul module autorisé à appeler libm)
    │   └── research/           # grid.rs (jalon 1.5), wfo.rs (jalon 2)
    ├── tests/                  # intégration : backtest sur fixture, double run identique, espion, perturbation, préfixe, allocateur compteur
    └── benches/engine.rs       # criterion : engine_2y_1m, sim_fills, pending_20 (20 limits en attente), indicators_1m, grid_32
```

Espace disque : `target/` mesuré à 1,2 Go en debug, 2,4 Go avec les tests, 0,4 Go en release ; prévoir ≥ 5 Go et le cache cargo en CI.

## 5. Schéma de données (jalon 1)

### 5.1 Bougie et fichier Parquet

| Colonne | Type Arrow | Note |
|---|---|---|
| `open_time_ms` | Int64 | UTC, début de bougie, trié strictement croissant, sans doublon |
| `open`, `high`, `low`, `close` | Float64 | prix en quote |
| `volume` | Float64 | en base |
| `trades` | UInt32 nullable | Kraken le fournit, OKX non (`Option<u32>` dans `Candle`) |

Un fichier = un mois = un row group (≈ 44 640 lignes en 1m), compression zstd, statistiques min/max par row group, `created_by = "tv4"`. Dans `parquet` 60.0.0 le plafond se règle par `set_max_row_group_row_count(Some(44_640))` (vérifié) ; il fixe un maximum, pas un découpage calendaire : on écrit explicitement un `RecordBatch` par mois. Objection tranchée : un row group par jour (1 440 lignes) multiplie les métadonnées sans bénéfice ; un fichier annuel à row groups mensuels serait équivalent en lecture mais rendrait la réécriture incrémentale OKX plus lourde (500 k lignes au lieu de 43 k). Le mois est l'unité de réécriture, donc le fichier **pour le 1m et le 5m** ; pour les TF ≥ 1h (1 à 720 lignes/mois) le fichier est annuel, sinon prolifération de fichiers minuscules qui ralentit le glob DuckDB.

Séquence durable d'écriture : `.tmp` → `File::sync_all` → `fs::rename` → `sync_all` sur le répertoire. Sans fsync, un rename survit à un crash du process mais pas à une coupure machine. Un seul écrivain par (exchange, symbole, TF) : deux processus (import Kraken + ingesteur OKX) qui renomment le même mois se perdent silencieusement les lignes de l'autre ; verrou fichier par partition dans `state/` (`fd-lock` ou `fs4`, à vérifier ; un `O_EXCL` maison suffit au jalon 1).

**« Immuable » ne vaut que pour les mois clos.** Le mois courant est mutable, donc son `content_hash` change à chaque ingestion : `tv4 dataset build` copie les fichiers dont le mois n'est pas clos dans `datasets/snapshots/<dataset_hash>/`, et `tv4 backtest` refuse de démarrer si un fichier listé dans le manifeste n'a plus le `content_hash` attendu.

Bougies absentes (Kraken : aucun trade) : **non synthétisées** à l'ingestion ; politique explicite au dataset (`gaps_policy = skip | ffill`, défaut `skip`), écrite dans le manifeste et donc dans `dataset_hash`. Objection tranchée : le défaut `ffill` d'iteration-first modifie silencieusement les indicateurs ; `skip` respecte la sémantique Kraken de 02-matrice. Le moteur traite un trou comme « le temps avance, pas le prix ».

Volumes : 1 paire × 2 ans × 1m = 1 051 200 lignes ≈ 50 Mo en RAM (SoA), **23-35 Mo sur disque mesurés** (zstd, prix synthétiques bruités ; les prix réels compressent mieux) ; 50 paires × 5 ans = 131 M lignes ≈ 2-3 Go sur disque, 6,3 Go en RAM si tout est chargé → traiter par paire. Lecture d'un mois : 2-4 ms ; de 5 ans (60 fichiers) : 226 ms.

### 5.2 Manifestes JSON

```jsonc
// data/candles/okx/BTC-USDT/1m/2025-03._manifest.json  (un par fichier Parquet ; BTC-USDT = référence publique liquide, le lieu d'exécution est BTC-USDC, §11 S7)
{ "exchange": "okx", "symbol": "BTC-USDT", "tf": "1m", "month": "2025-03", "closed": true,
  "row_count": 44640, "ts_min": 1740787200000, "ts_max": 1743465540000,
  "content_hash": "blake3:…", "hash_format": "dsh-v1", "source": "okx-history-candles", "ingested_at": "…",
  "continuity": { "gaps": 3, "longest_gap_ms": 180000, "duplicates": 0, "rejected_unclosed": 1, "ohlc_errors": 0 } }

// datasets/btcusd_2y.json  (dataset figé, immuable)
{ "name": "btcusd_2y", "exchange": "kraken", "symbols": ["BTC-USD"], "tfs": ["1m"],
  "window": ["2024-01-01T00:00:00Z", "2026-01-01T00:00:00Z"], "gaps_policy": "skip",
  "files": [ { "path": "…/2024-01.parquet", "content_hash": "blake3:…", "row_count": 44640 }, … ],
  "row_count": 1051200, "dataset_hash": "blake3:…", "hash_format": "dsh-v1", "built_at": "…", "tv4_version": "0.1.0" }

// runs/<run_id>/manifest.json
{ "run_id": "…", "config_hash": "…", "dataset_hash": "…", "dataset": "btcusd_2y",
  "strategy": { "id": "sma_cross", "version": "1.0.0", "params": { … } },
  "fees": { "exchange": "okx", "maker_bps": 20, "taker_bps": 35 }, "slippage_bps": 5,
  "sim_rules": { "version": 1, "intrabar_rule": "stop_first", "limit_fill": "min_limit_open", "tp_fill": "maker" },
  "engine": { "tv4_version": "0.1.0", "git_sha": "…", "rustc": "1.97.0", "target": "x86_64-unknown-linux-gnu",
              "target_cpu": "x86-64-v3", "binary_hash": "blake3:…" },
  "window": [ "…", "…" ], "warmup_bars": 50, "started_at": "…", "duration_ms": 412,
  "result_hash": "blake3(trades.csv)" }
```

### 5.3 Hashes

- `content_hash` d'un fichier et `dataset_hash` sont calculés sur un **encodage canonique possédé par le projet, `dsh-v1`**, spécifié dans `docs/dataset-hash.md`. **Correction (réfutation)** : « hasher le contenu logique plutôt que les octets Parquet » ne *garantit* la stabilité que si l'encodage ne dépend ni des octets Parquet (le footer contient `created_by = "parquet-rs version 60.0.0"`, donc le hash d'octets change à chaque montée de version), ni des buffers Arrow (reproduit : hacher `data.buffers()` + bitmap de validité donne un hash différent selon le `batch_size` 1024/4096/5000 et selon le fichier, à données identiques), ni d'un en-tête dérivé de `format!("{schema:?}")` ou des métadonnées `ARROW:schema`. Règles : en-tête littéral écrit à la main (`b"tradingv4-dataset-v1|exchange|symbol|tf|window|gaps_policy|row_count"`) ; lignes en ordre canonique (tri par `open_time_ms`, dédoublonné) ; par ligne, `open_time_ms` (i64 LE), puis `open, high, low, close, volume` (bits f64 LE, `-0.0` et NaN canonicalisés, NaN rejeté à l'ingestion), puis `trades` (1 octet de présence + u32 LE) ; un seul `blake3::Hasher` incrémental, sans marqueur de batch ni de row group. Coût mesuré : 5,7 ms par mois. **Test golden en CI** : fixture Parquet minuscule + `dataset_hash` attendu en dur ; toute montée d'arrow/parquet qui le casse est bloquée. Changer le format = `dsh-v2`, jamais de recalcul silencieux. Un hash physique (sha256 du fichier) peut être conservé à part pour l'intégrité de transfert, clairement distingué.
- `config_hash = blake3(JSON à clés triées via serde_json::Value)` de `{ strategy.id, strategy.version, params, fees, slippage_bps, sim_rules, warmup, min_trades, engine.tv4_version }`.
- `run_id = blake3(config_hash ‖ dataset_hash ‖ window)`.
- `result_hash = blake3(trades.csv)` ; deux runs à (`config_hash`, `dataset_hash`, `engine`) égaux doivent donner le même `result_hash` ; `tv4 verify runA runB` compare `metrics.json`, `trades.csv` et `equity.csv` à l'octet.

### 5.4 Jalon 2 : PostgreSQL

Les tables `datasets(id, name, exchange, symbols, tfs, window_start, window_end, gaps_policy, dataset_hash, row_count, manifest jsonb)` et `runs(id, dataset_id, strategy_id, strategy_version, params jsonb, fees jsonb, config_hash, engine jsonb, metrics jsonb, verdict, trades_path, equity_path, started_at, duration_ms)` sont **remplies par import des manifestes JSON** (Doctrine, jalon 2). Le `PgSink` (sqlx 0.9, COPY via `copy_in_raw`) pour les bougies et l'hypertable Timescale `candles` n'arrivent qu'avec un besoin de requêtes temps réel sur bougies (jalon 3) ; d'ici là DuckDB CLI sur les Parquet répond à toute question ad hoc (mesuré : count/min/max sur 12 mois en 14 ms, trous par window function en 9 ms, resampling 1m → 1h en 23 ms).

## 6. Interface `Strategy` en Rust

```rust
// tv4/src/model.rs
pub type Ts = i64;                                   // epoch ms UTC

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Candle {
    pub open_time_ms: Ts,
    pub open: f64, pub high: f64, pub low: f64, pub close: f64,
    pub volume: f64,                                 // en base
    pub trades: Option<u32>,                         // Kraken oui, OKX non
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Timeframe { M1, M5, M15, M30, H1, H4, H12, D1 }
impl Timeframe { pub const fn ms(self) -> i64 { /* 60_000, 300_000, … */ } }

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub struct FeeSchedule { pub exchange: ExchangeId, pub maker_bps: f64, pub taker_bps: f64 }

/// Entrée du simulateur. Jalon 1 : seul `ClosedBar` est émis ; les variantes `Trade`/`BookTop`
/// sont réservées pour le paper (jalon 3) afin que le simulateur reste unique.
#[derive(Clone, Copy, Debug)]
pub enum MarketEvent { ClosedBar(Candle) /* , Trade { ts, px, qty }, BookTop { ts, bid, ask } */ }

// tv4/src/strategy/mod.rs
#![forbid(unsafe_code)]
pub enum IndicatorKind { Sma { period: usize }, Ema { period: usize }, Rsi { period: usize }, Atr { period: usize }, Vwap }
pub struct IndicatorSpec { pub name: &'static str, pub tf: Timeframe, pub kind: IndicatorKind }

#[derive(Clone, Copy, Debug)] pub enum OrderKind { Market, Limit { price: f64, ttl_bars: u32 } }
#[derive(Clone, Copy, Debug)] pub enum Size { PctEquity(f64), RiskPct(f64), Quote(f64) }  // RiskPct exige un stop
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Side { Buy, Sell }
#[derive(Clone, Copy, Debug)]
pub enum Signal {
    EnterLong { size: Size, kind: OrderKind, stop: Option<f64>, take_profit: Option<f64>, tag: &'static str },
    ExitLong  { kind: OrderKind, tag: &'static str },   // sort TOUTE la position
    MoveStop  { stop: f64 },                            // pose ou déplace le stop de la position agrégée
    CancelPending,                                      // annule TOUS les ordres en attente
    /// Limit autonome, achat ou vente partielle, qui coexiste avec les autres ordres en attente
    /// (décision du 2026-10-13 : une stratégie peut tenir plusieurs limits à la fois, cf. §15.3).
    /// `slot` identifie le niveau pour la stratégie (ex. index de grille) ; unique parmi les ordres en attente.
    /// Pas de stop attaché : `Size::RiskPct` est refusé ici (rejet compté), `PctEquity` ou `Quote` seulement.
    PlaceLimit { side: Side, price: f64, size: Size, ttl_bars: u32, slot: u16, tag: &'static str },
    CancelSlot { slot: u16 },
}

#[derive(Clone, Copy, Debug)]
pub struct Position { pub qty: f64, pub avg_price: f64, pub stop: Option<f64>, pub take_profit: Option<f64>, pub opened_at_ms: Ts }

/// Ordre en attente, `Copy` (pas de `String`). `slot` = 0 pour les ordres issus de `EnterLong`/`ExitLong`.
#[derive(Clone, Copy, Debug)]
pub struct PendingOrder { pub side: Side, pub kind: OrderKind, pub qty: f64, pub slot: u16, pub tag: &'static str, pub placed_at_ms: Ts, pub ttl_left: u32 }

/// Vue en lecture seule construite par le moteur. Toutes les tranches s'arrêtent à la dernière
/// bougie CLÔTURÉE du timeframe demandé à l'instant `now_ms` (convention : une bougie TF est
/// clôturée quand `open_time + ms(tf) <= now_ms`, donc la H1 10:00 est visible dans l'appel du
/// 1m 10:59). Champs de données PRIVÉS, accesseurs bornés : pas de voie facile vers le futur.
/// Une seule lifetime visible : `'a` = durée de vie des colonnes du dataset.
pub struct BarContext<'a> {
    pub now_ms: Ts,                    // close time de la bougie 1m courante
    pub symbol: &'a str,
    pub equity: f64,                   // cash + qty × close précédent
    pub quote_free: f64,               // cash − quote réservé par les limits d'achat en attente
    pub qty_free: f64,                 // qty − quantité réservée par les limits de vente en attente
    pub position: Option<Position>,    // Copy
    pub pending: &'a [PendingOrder],   // ≤ `max_pending` (SimRules)
    pub fees: FeeSchedule,
    views: &'a [TfView<'a>],           // une par timeframe déclaré
    just_closed: u8,                   // bitmask des TF qui viennent de clôturer sur cette 1m
}
impl<'a> BarContext<'a> {
    pub fn candles(&self, tf: Timeframe) -> &'a [Candle];        // clôturées, chronologiques
    pub fn ind(&self, tf: Timeframe, name: &str) -> &'a [f64];   // même longueur que candles(tf), NaN en warmup
    pub fn just_closed(&self, tf: Timeframe) -> bool;
    pub fn last(&self, tf: Timeframe) -> Option<&'a Candle> { self.candles(tf).last() }
    /// Niveau déjà posé ? Évite qu'une stratégie à plusieurs limits reposte un slot ouvert.
    pub fn pending_slot(&self, slot: u16) -> Option<&'a PendingOrder> { self.pending.iter().find(|o| o.slot == slot) }
}

pub trait Strategy: Send {
    fn id(&self) -> &'static str;                     // "sma_cross"
    fn version(&self) -> &'static str;                // "1.0.0", entre dans config_hash
    fn params_json(&self) -> serde_json::Value;       // entre dans config_hash
    fn timeframes(&self) -> &[Timeframe];             // ex. &[H1, M15]
    fn indicators(&self) -> Vec<IndicatorSpec>;       // déclaratif, précalculé par le moteur
    fn warmup_bars(&self) -> usize;                   // en bougies du PLUS GRAND TF déclaré
    /// Appelée uniquement sur les bougies 1m où un TF déclaré vient de clôturer.
    /// `out` est réutilisé par le moteur (clear() avant l'appel) : zéro allocation par bougie.
    fn on_bar(&mut self, ctx: &BarContext<'_>, out: &mut Vec<Signal>);
}
```

Règles :

- **`Send` dès maintenant** (gratuit) : interdit `Rc`/`RefCell` dans l'état d'une stratégie. **Correction (réfutation)** : `Send` sur le trait n'est ni nécessaire ni suffisant pour la grille rayon du jalon 1.5 ; rayon exige `Sync + Send` sur la *fermeture* (donc sur `Arc<Dataset>`, le cache d'indicateurs, la fabrique du registre) et `Send` sur le résultat. Ce qui permet la greffe sans refonte, c'est que `engine::run_cell` soit une fonction pure (pas de `static mut`, `thread_local!`, RNG globale, journal partagé), que `Dataset` et le cache soient immuables et `Sync`, et que les sorties par cellule soient réduites dans le worker. `Send` reste comme garde-fou bon marché.
- **`ind(tf, name)` résout `name` à la construction du contexte, pas par bougie** : le moteur indexe les specs une fois (`Vec` dans l'ordre de `indicators()`), l'accès par bougie est un index. Mesuré : une résolution `HashMap<String, _>` par appel coûte 72-141 ns/bougie, soit ×5 à ×10.
- **`warmup_bars` en bougies du plus grand TF**, défini une fois pour toutes (les trois propositions divergeaient). Le moteur étend la fenêtre en amont et **refuse de démarrer** si un indicateur est encore NaN à `from`.
- **Lifetime** : une seule, `BarContext<'a>`. **Correction (réfutation)** : la variante sans lifetime (`BarContext { cols: Arc<Columns>, upto: usize }`, champs privés, accesseurs bornés) offre **exactement la même borne d'accès** (vérifié : mêmes signaux au bit près, une seule allocation sur 180 000 bougies dans les deux variantes) ; le lifetime n'ajoute qu'une interdiction de *retenir* le contexte au-delà de `on_bar`, qui n'a pas de rapport avec le look-ahead. Le choix est donc ergonomique : `BarContext<'a>` par défaut au jalon 1 (zéro `Arc`), bascule vers la variante `Arc` + indices si elle bloque en semaine 3 ou si un état incrémental derrière `Box<dyn Strategy>` la rend pénible, **sans perte de garantie**. Dans les deux cas : la stratégie ne stocke jamais de référence issue du contexte, seulement des `f64` copiés.
- **Plusieurs ordres en attente** (décision du 2026-10-13, §15.3) : `PlaceLimit`/`CancelSlot` s'ajoutent sans changer la sémantique de `EnterLong`/`ExitLong`/`MoveStop`/`CancelPending` ; `sma_cross` et toute stratégie à position unique ne voient pas la différence. Une stratégie qui pose plusieurs niveaux lit `ctx.pending` (ou `pending_slot`) pour ne pas reposter un niveau ouvert, et `quote_free`/`qty_free` pour ne pas poser au-delà du disponible. Le moteur ne connaît aucun concept de « grille » : c'est une stratégie comme une autre, qui émet des `PlaceLimit`.
- **Paramètres** : struct `serde::Deserialize` par stratégie, chargée depuis TOML, resérialisée via `serde_json::to_value` (clés triées) pour le hash.
- **Registre** : `match` explicite, pas de plugin dynamique ; fabrique `Fn(&toml::Value) -> Result<Box<dyn Strategy>> + Send + Sync`.

```rust
// tv4/src/strategy/sma_cross.rs
#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
pub struct SmaCrossParams { pub tf: Timeframe, pub fast: usize, pub slow: usize, pub stop_atr_mult: f64 }

pub struct SmaCross { p: SmaCrossParams, prev_diff: Option<f64> }

impl Strategy for SmaCross {
    fn id(&self) -> &'static str { "sma_cross" }
    fn version(&self) -> &'static str { "1.0.0" }
    fn params_json(&self) -> serde_json::Value { serde_json::to_value(&self.p).expect("params sérialisables") }
    fn timeframes(&self) -> &[Timeframe] { std::slice::from_ref(&self.p.tf) }
    fn indicators(&self) -> Vec<IndicatorSpec> { vec![
        IndicatorSpec { name: "fast", tf: self.p.tf, kind: IndicatorKind::Sma { period: self.p.fast } },
        IndicatorSpec { name: "slow", tf: self.p.tf, kind: IndicatorKind::Sma { period: self.p.slow } },
        IndicatorSpec { name: "atr",  tf: self.p.tf, kind: IndicatorKind::Atr { period: 14 } },
    ] }
    fn warmup_bars(&self) -> usize { self.p.slow.max(14) }
    fn on_bar(&mut self, ctx: &BarContext<'_>, out: &mut Vec<Signal>) {
        if !ctx.just_closed(self.p.tf) { return; }
        let (Some(&f), Some(&s), Some(&atr), Some(c)) =
            (ctx.ind(self.p.tf, "fast").last(), ctx.ind(self.p.tf, "slow").last(),
             ctx.ind(self.p.tf, "atr").last(), ctx.last(self.p.tf)) else { return };
        let d = f - s;
        match (self.prev_diff, ctx.position.is_none()) {
            (Some(p), true)  if p <= 0.0 && d > 0.0 => out.push(Signal::EnterLong {
                size: Size::RiskPct(0.01), kind: OrderKind::Market,
                stop: Some(c.close - self.p.stop_atr_mult * atr), take_profit: None, tag: "cross_up" }),
            (Some(p), false) if p >= 0.0 && d < 0.0 => out.push(Signal::ExitLong { kind: OrderKind::Market, tag: "cross_down" }),
            _ => {}
        }
        self.prev_diff = Some(d);
    }
}

// tv4/src/strategy/registry.rs
pub fn build(id: &str, params: &toml::Value) -> anyhow::Result<Box<dyn Strategy>> {
    Ok(match id {
        "sma_cross" => Box::new(SmaCross::new(params.clone().try_into::<SmaCrossParams>()?)),
        other => anyhow::bail!("stratégie inconnue : {other}"),
    })
}
```

`Signal` est une intention ; le simulateur décide des fills. La stratégie ne touche jamais aux ordres. Ce trait est volontairement neutre (`params_json`, pas de type associé) : une implémentation scriptée pourrait être ajoutée plus tard sans toucher au moteur ; elle n'est pas construite.

**Écarts d'implémentation (S2).** Le code de `tv4/src/strategy/` diffère de l'extrait ci-dessus ; la liste complète, justifiée, est en tête de `tv4/src/strategy/mod.rs`. En bref : `BarContext` n'a plus de champ public (accesseurs bornés, scalaires regroupés dans `BarState`, `ctx.position()` au lieu de `ctx.position`) ; les colonnes d'un timeframe sont rangées une fois, à la construction, dans l'ordre de `indicators()` ; `ind(tf, name)` y cherche le nom parmi les quelques noms du timeframe (comparaison de `&'static str`, sans hachage ni allocation) et `ind_at(tf, index)` donne l'accès direct par index ; `TfView` garde les colonnes complètes et un compteur `closed` ; le masque `just_closed` est un `u8` construit avec `Timeframe::bit()` ; les erreurs sont typées (`StrategyError`) et `registry::build` rend un `Result` de cette erreur, pas `anyhow`. `BarState`, `TfView` et `BarContext` sont définis dans un sous-module privé `strategy/context.rs` : un champ privé reste lisible par les modules descendants de celui qui définit le type, donc définis dans `strategy/mod.rs` ils auraient été lisibles par `sma_cross` et par toute stratégie future (lecture au-delà de `closed` sans `unsafe`). `tests/architecture.rs` interdit l'accès à ces champs hors de `context.rs`.

## 7. Boucle de backtest et simulateur

### 7.1 Préparation (une fois par run, ou partagée par campagne)

1. Charger les colonnes 1m du dataset depuis `from − warmup_bars × ms(TF max)` jusqu'à `to` (projection des 6 colonnes, filtre de fichiers par mois puis de row groups par stats, calculé par nous et passé à `with_row_groups`). Vérifier le `content_hash` de chaque fichier contre le manifeste. `Arc<Dataset>` en campagne.
2. `mtf` agrège vers chaque TF déclaré : `Vec<Candle>` par TF + index `closed_count[tf][i]` (nombre de bougies TF clôturées à la fin de la 1m `i`, **calculé par timestamps** `open_time_tf + ms(tf) ≤ open_time_1m[i] + 60 000`, jamais par arithmétique d'indices, fausse avec les trous Kraken) et bitmask `just_closed[i]`. Frontières UTC : une H1 se clôture sur la 1m `hh:59`. Les trous 1m restent des trous. Les bougies TF incomplètes en fin de fenêtre ne sont jamais exposées. Mesuré : agrégation à la volée 1m → 15m + 1h sur 2 ans = +6 ms.
3. `indicators` calcule chaque `IndicatorSpec` sur sa série TF par fold de `update` : `Vec<f64>` de même longueur, NaN en warmup, une fois, jamais par bougie (inverse de V3). Cache mémoire par (TF, spec) pendant une campagne, **précalculé avant la grille** à partir de l'ensemble des paramètres (immuable, `Sync`), avec un budget mémoire explicite : `colonnes distinctes × bougies × 8 octets` (mesuré : 368 périodes SMA distinctes × 2 ans = 3,1 Go).
4. Refus de démarrer si une valeur visible à `from` est NaN.
5. `Simulator::new(fees, slippage, rules)`, `Vec<Signal>` et `Vec<PendingOrder>` (capacité `rules.max_pending`, défaut 64, dans `config_hash`) alloués une fois, `equity: Vec<f64>` préalloué.

### 7.2 Boucle, pour `i` sur les 1m de `[start, n)`

| Étape | Action | Règle |
|---|---|---|
| 1. Ouverture | Ordres en attente émis à `i−1` ou avant, jamais à `i` ; jusqu'à `max_pending` ordres parcourus, suppression par `swap_remove` | Market : fill à `open[i] × (1 ± slippage)`, taker. Limit achat : rempli si `low[i] ≤ limite` au prix `min(limite, open[i])` (gap sous la limite = meilleur prix), maker. Limit vente (symétrique) : rempli si `high[i] ≥ limite` au prix `max(limite, open[i])` (gap au-dessus = meilleur prix), maker. Un achat et une vente touchés dans la même 1m sont tous deux remplis à leur limite (comme sur l'exchange) ; mais un ordre posé par `on_bar` à `i` n'est exécutable qu'à partir de `i+1` (I3), donc pas d'enchaînement achat → repose → vente dans la même bougie. TTL décrémenté, annulé à expiration. Les règles de 04 §3.1 (traversée d'un tick, délai d'activation) priment là où elles diffèrent, cf. §15.1 ligne 5 |
| 2. Intrabar | Stop et take-profit de la position contre `[low[i], high[i]]` | Stop : fill à `stop × (1 − slippage)`, taker. TP : fill à `tp`, maker. Les deux touchés dans la même bougie : `IntrabarRule::{StopFirst, TpFirst}`, défaut `StopFirst` (pessimiste), dans `config_hash` |
| 3. Mark-to-market | `equity[i] = cash + qty × close[i]`, pic et drawdown courant, MFE/MAE de la position ouverte | Écriture dans un `Vec<f64>` préalloué |
| 4. Stratégie | Si `just_closed[i] ≠ 0` **et** `i` dans la fenêtre de résultats : construire `BarContext` (sur la pile, tranches `[..closed_count]`), `signals.clear()`, `strategy.on_bar(&ctx, &mut signals)` | Une stratégie 15m est appelée une 1m sur 15, le simulateur tourne à chaque 1m |
| 5. Ordres | Signaux → ordres en attente, exécutables à partir de `i+1` ; sizing `PctEquity`, `RiskPct` = `(equity × pct) / (entry − stop)`, `Quote` ; arrondi lot/tick, minimum de notional par paire (fichier d'instruments hashé, §15.1 ligne 6) | Spot : une limit d'achat **réserve son notional à la pose** (`quote_reserved`, comme OKX), une limit de vente réserve sa `qty` ; achat refusé et compté si `quote_free` insuffisant, vente si `qty_free == 0` ; `EnterLong` avec position existante ignoré et compté (`signals_rejected`) ; `PlaceLimit` refusé et compté si `slot` déjà ouvert, si `RiskPct`, ou si `pending.len() == max_pending`. Les fills partiels de `PlaceLimit` font monter et descendre `qty` ; `avg_price` recalculé à l'achat ; `MoveStop` et le TP s'appliquent à la position agrégée. Jamais de fill à `close[i]` : c'est le prix que la stratégie vient de voir |
| 6. Journal | Chaque fill → `Trade { entry_ts, exit_ts, qty, entry_px, exit_px, fees, slippage, pnl_net, mfe, mae, reason, tag }` (FIFO **par lot** : une vente partielle clôt les lots les plus anciens) | `Vec<Trade>`, croissance amortie |

Fin de fenêtre : position encore ouverte → fermée au dernier close, `forced_close = true` dans `trades.csv`, comptée dans un PnL « marked » séparé du PnL réalisé.

Allocation : zéro `Vec::new`/`Box`/`String` dans les étapes 1 à 6 (`PendingOrder` et `Trade` sont `Copy`, tags `&'static str`). Un test d'intégration avec allocateur compteur vérifie « ≤ 1 allocation pour 10 000 bougies » en CI (mesuré : un `Vec::with_capacity` par bougie coûte +8 ns, un retour `Vec<Signal>` +6 à +8 ns). Pas de dogme au-delà : le profil criterion décide.

### 7.3 Verdict et rapports

`metrics.json` : PnL net, PnL marked, expectancy nette, profit factor, max drawdown sur la série 1m et sur la série 1h (equity aux clôtures 1h, quel que soit le pas `--equity-step` : même résolution que l'equity de porte du live, 08 §B.4), nb trades, win rate, durée moyenne, MFE/MAE moyens, frais totaux, slippage total, `signals_rejected`, `forced_close`, et `verdict`: `"ok"` ou `"rejected_negative_expectancy"` ou `"rejected_too_few_trades"` (expectancy nette ≤ 0 **ou** trades < `min_trades`, défaut 30). Un verdict rejeté est un résultat, pas une absence de résultat. `equity.csv` est échantillonné par défaut (`--equity-step 1h`, 17 520 lignes pour 2 ans, ~1 ms mesuré) ; `--equity-step 1m` sur demande (1 M lignes ≈ 35 Mo, 0,15-0,5 s mesuré). Objection tranchée : l'option existe, le défaut est l'échantillonnage.

### 7.4 Reproductibilité

**Correction (réfutation)** : les deux prémisses sont confirmées empiriquement (rustc 1.97 ne contracte pas `a*b+c` en FMA même avec `-C target-feature=+fma` ou `target-cpu=native`, vérifié à l'assembleur ; RFC 3514 : IEEE 754 arrondi au plus proche, pas de fast-math ; SMA/EMA/RSI/ATR/VWAP n'appellent pas libm ; EMA sur 2 M de points identique au bit entre SSE2 et `native`). Mais « même binaire ⇒ même résultat » **n'est vrai que si l'ordre des opérations est déterministe**, et deux contre-exemples le cassent avec le même binaire : `v.par_iter().sum::<f64>()` donne 26 à 28 résultats binaires distincts sur 30 appels (work-stealing), et `HashMap<_, f64>::values().sum()` change à chaque process (`RandomState`). Règles, toutes vérifiées par clippy ou par test :

- Le chemin de calcul (indicateurs, simulateur, agrégations) est **mono-thread par run** ; le parallélisme ne porte que sur des cellules indépendantes dont les résultats sont fusionnés dans un ordre fixe. `par_iter().sum()/reduce()` sur des flottants : interdit (`disallowed_methods`).
- Aucune `HashMap`/`HashSet` std itérée dans le calcul (`disallowed_types` dans `engine`, `sim`, `indicators`) : `Vec`, `BTreeMap`, ou `Vec` trié par symbole. Tris : `sort_by` avec clé de départage, jamais `sort_unstable` sur des égalités métier.
- Aucune horloge système dans le calcul.
- `ln`/`exp`/`powf` (log-returns, annualisation, Sharpe) passent par la libm de la glibc, qui sélectionne à l'exécution des variantes FMA/non-FMA (927 `vfmadd` dans `libm.so.6` de la glibc 2.39 du conteneur) : **confinés dans `report`**, comparés en CI à tolérance ULP documentée, ou remplacés par la crate `libm` pure Rust (à vérifier) si l'égalité à l'octet entre machines devient nécessaire. `sqrt`, `floor`, `round` sont exacts.
- `mul_add` explicite autorisé seulement si la cible a `+fma` (sinon `fma` logiciel glibc, déterministe mais lent) ; `clippy::suboptimal_flops` désactivé.
- `-C target-cpu` fixe dans `.cargo/config.toml` (`x86-64-v3`), jamais `native` ; `rustc -vV`, `target_cpu` et le hash du binaire dans le manifeste. Une dépendance faisant du dispatch runtime (`is_x86_feature_detected!`) reste un risque entre deux runners, listé en §12.
- NaN et `-0.0` canonicalisés avant hash (payload NaN non garanti par le langage) ; NaN rejeté à l'ingestion.
- CI : le backtest de fumée (fixture 10 000 bougies) tourne deux fois **dans le même job et en deux process**, `tv4 verify` exige l'égalité à l'octet ; un golden file versionné détecte en plus les régressions entre commits, que le double run ne voit pas.

### 7.5 Multi-timeframe sans look-ahead : quatre invariants, cinq tests

**Correction (réfutation)** : « impossible par construction » est remplacé par « bornage d'index sous invariants testés ». Deux contre-exemples compilés sans warning fuient à travers une tranche bornée : un z-score 1h normalisé sur tout l'échantillon (perturber les 10 dernières bougies du dataset change le signal au milieu), et un `closed_count` calculé sur `open_time_1h ≤ open_time_1m` (176 856 bougies sur 179 838 voient la H1 en cours). Un proptest naïf « j aléatoire dans (i, len) » passe **au vert** sur ce second bug (0 détection sur 256 cas) ; un générateur ciblé en trouve 111/256. Les tests ne prouvent pas une absence : ce sont des détecteurs, et le document le dit.

| Invariant | Test |
|---|---|
| I1. **Causalité des indicateurs** (stabilité par préfixe) : pour tout k, `indic(data[..k]) == indic(data)[..k]` au bit | Test déterministe sur une grille de k (puissances de 2, bornes de fenêtre, k aléatoires), sur **chaque** noyau d'indicateur ; interdiction par revue de `center`, `shift` négatif, normalisation plein échantillon. Garanti structurellement par le fold de `update` (§1), vérifié quand même |
| I2. **Visibilité MTF** : `closed_count[tf][i]` défini par `open_time_tf + ms(tf) ≤ close_time_1m[i]` | **Espion** avec oracle indépendant : dataset synthétique où chaque valeur vaut son propre timestamp, stratégie de test qui exige `max(timestamp vu) ≤ now_ms` sur tous les TF, et `n_attendu` recalculé par timestamps (jamais en réutilisant `closed_count`, sinon on teste une fonction contre elle-même) |
| I3. **Timing de fill** : un signal émis à `i` ne produit jamais un fill avant l'open de `i+1` | « Signal sur la dernière bougie → zéro fill » et « `fill.price ∈ [low, high]` de `i+1` » |
| I4. **Non-interférence** | **Perturbation** (proptest, `cases ≥ 1 000`) avec générateur **ciblé** : `i` aléatoire, `j ∈ (i, i + ms(tf_max)]` (pas uniforme sur le futur), perturbation de `high` **et** `low` (perturber `close` seul ne change la H1 que sur sa dernière minute : 3/256 détections mesurées), agrégats et indicateurs **recalculés**, stratégie canari qui hache **tout** le visible (longueurs + toutes les tranches), signaux et fills identiques jusqu'à `i`. Plus un cas « perturber les k dernières bougies du dataset » contre les fuites globales, et des trous 1m injectés. **Test mutant** en CI : réintroduire volontairement le `closed_count` faux et vérifier que le test rougit |
| (Agrégation, hors look-ahead) | **Oracle** : l'agrégation 1m → 5m/1h est comparée aux fichiers Kraken 5m/1h natifs sur la fixture (valide l'agrégation et la gestion des bougies manquantes, pas la frontière de clôture) ; l'agrégat 15m est une fonction des 15 bougies 1m (proptest) |

Convention de frontière, décision explicite : la H1 `10:00` est visible dans l'appel `on_bar` du 1m `10:59` (même instant de clôture), optimiste de quelques centaines de ms par rapport au WS réel ; `ctx.just_closed(tf)` évite aux stratégies de recalculer sur une H1 inchangée. Rien dans le système de types n'empêche une stratégie d'ouvrir le Parquet par `std::fs` : `#![forbid(unsafe_code)]` et la revue couvrent le bug accidentel, pas l'adversaire.

### 7.6 Campagnes (`research`, jalon 1.5 / 2)

`tv4 grid --params-grid grid.toml` : `Arc<Dataset>` chargé une fois, cache d'indicateurs **précalculé avant la grille** (immuable derrière `Arc`, jamais `Mutex<HashMap>` paresseux : mesuré ×3 de perte par contention), `rayon::par_iter` sur les cellules (une cellule = une stratégie construite par le registre + un `Simulator`, via `engine::run_cell` pur), résultats **réduits dans le worker** (métriques + trades, equity échantillonnée ; jamais 1 000 courbes complètes collectées : 16 Mo × 1 000 = 16 Go) et **collectés dans l'ordre de la grille** (`par_iter().map().collect()` conserve l'ordre ; `campagne.csv` déterministe). **Correction (réfutation)** : le scaling mesuré est de **×2,2 à ×4,0 sur 4 cœurs** selon que la cellule est compute-bound ou memory-bound et selon la durée de la charge (×3,5 en charge courte, ×2,2-2,4 en charge soutenue sur VM) ; 32 cellules × 2 ans en **50-140 ms**, **1 000 cellules en 1,5 à 5 s pour `sma_cross`**, ~10-15 s pour une stratégie à 10 colonnes d'indicateurs, 40-60 s à 100 ns/bougie. Le coût par cellule se mesure au jalon 1 (criterion `grid_32`), il ne s'extrapole pas. Budget mémoire du cache : `colonnes distinctes × bougies × 8 octets` ; au-delà de quelques centaines de Mo, découper la grille en lots ou calculer l'indicateur à la volée dans la cellule (SMA en somme glissante : +15-25 % mesuré). Test CI : même grille à 1 et 4 threads, résultats identiques au bit (vérifié). Walk-forward : `WfoPlan` (fenêtres train/test, holdout exclu par construction) hashé, jalon 2.

## 8. Performance : mesures et cibles

| Poste (2 ans BTC/USD 1m = 1 051 200 bougies, 1 cœur, release) | Mesuré (conteneur 4 vCPU Xeon 2,1 GHz, reproduit 2× par la réfutation) | Cible retenue |
|---|---|---|
| Boucle nue (SoA, SMA, market, frais, equity) | 2,6-8 ns/bougie ; plancher lecture mémoire 3,7-4,4 ns | — |
| Boucle complète (`Box<dyn Strategy>`, 2 TF via `closed_count`, indicateurs, limit TTL, stop/TP, MFE/MAE, journal, equity) | **10-14 ns/bougie = 11-15 ms** (6-18 ns selon CPU et bruit ; ×3 en scalping à 1 trade / 4 bougies = 35 ms ; 12-18 ns sur 10 ans hors cache L3) | < 50 ns/bougie, seuil criterion en CI (détecte une allocation ou un hachage par bougie) |
| Même boucle avec 20-40 limits en attente (stratégie de type grille) | **Estimation, pas une mesure** : une comparaison `f64` par ordre et par 1m, +20 à +60 ns/bougie, soit ×3 à ×5 pour cette stratégie seule ; coût nul sans `PlaceLimit` (boucle vide) | Bench criterion `pending_20` à écrire en S6 ; reste sous le seuil de 50 ns/bougie pour les stratégies sans limits multiples |
| Même boucle, SDK « Python-like » (`HashMap<String,f64>` par bougie, fenêtre de 200 bougies copiée, ou recalcul d'indicateur dans `on_bar`) | 72-203 ns/bougie : c'est l'origine des estimations « 100-300 ns », valables pour ce design-là | Interdit par le principe 8 |
| Build debug sans `opt-level` (profil dev par défaut) | 82-121 ns/bougie, run complet ~1 s (**×3-4**, pas ×10-30 : le décodage Parquet non optimisé domine) | `[profile.dev] opt-level = 1` + `[profile.dev.package."*"] opt-level = 3` : run dev mesuré 0,19-0,43 s, indiscernable du release |
| Lecture Parquet 6 colonnes | **60-150 ms à chaud selon le codec** (zstd 115-146), ~350 ms à froid | < 300 ms à chaud |
| Écriture Parquet 2 ans | 0,54 s | une fois à l'ingestion |
| Réécriture atomique d'un mois (44 640 lignes, zstd, fsync, rename) | 30-60 ms ; lecture d'un mois 2-4 ms | tamponner les pages OKX, réécrire par lot |
| Hash canonique `dsh-v1` | 5,7 ms par mois (blake3) ; sha256 d'un fichier de 50 Mo : 75 ms | hash logique uniquement |
| SMA/EMA/RSI/ATR sur 1,05 M points | 6-16 ms chacun | < 20 ms par indicateur |
| Resampling 1m → 1h | 6-11 ms | < 30 ms |
| `equity.csv` 1 M lignes / échantillonné 1h | 0,15-0,5 s / ~1 ms | échantillonné par défaut |
| **Run `tv4 backtest` mur-à-mur** | **0,20-0,38 s mesurés** (`time`, process complet, trades.csv + metrics.json inclus) ; 0,42 s à froid | **< 5 s** (objectif officiel < 60 s) |
| Build froid arrow + parquet + clap + serde + csv + sha2 (145 crates) | debug **47-67 s**, release **2 min 07 s - 2 min 34 s** (6 min 48 s sur conteneur chargé) ; incrémental debug 1,0-1,4 s, release 2,6-3,4 s ; premier `cargo check` 14 s puis 0,2 s | acceptable ; `sccache` à mesurer sur ton poste |
| Grille 32 cellules × 2 ans, 4 cœurs | 50-140 ms (×2,2 à ×4,0) | — |
| Grille 1 000 cellules × 2 ans, 4 cœurs | 1,5-5 s (`sma_cross`), 10-15 s (10 colonnes), 40-60 s (100 ns/bougie) | mesurer la stratégie réelle avant toute promesse |
| 50 paires × 5 ans, une stratégie | 131 M × 12 ns ≈ 1,6 s de boucle + 10-15 s de lecture, par paire | traiter par paire, pas en bloc |
| Ingestion OKX 1 an de 1m | **≈ 11 min à 8 req/s effectifs avec 3 threads** (mesuré 7,95 req/s sur serveur mock à 250 ms de RTT) ; **22-33 min en mono-thread** (borné par le RTT, pas par le code) | 3 threads std + token bucket ; ≈ 4 min avec `limit=300` (accepté : vérifié le 13/10, §11 S7) |
| Ingestion Kraken 2 ans CSV → Parquet | estimation 1-3 s (à valider) | — |

Objection tranchée : les estimations « 100-300 ns/bougie » des propositions ne sont pas absurdes, elles décrivent un SDK « Python-like » (lookup par nom, copie de fenêtre, recalcul dans `on_bar`, build debug, `rust_decimal` dans la boucle) ; le chiffre de travail du design retenu est **10-14 ns mesuré deux fois**, ce qui enterre monomorphisation (≤ 2 ns), SmallVec, mimalloc (aucun gain mesuré), `target-cpu=native` (≤ 20 %) et zéro-allocation dogmatique. Les « 4 vCPU » ne contribuent pas à la boucle, strictement séquentielle ; ils servent aux grilles. Ce qui casse la marge n'est pas le moteur mais une stratégie qui recalcule un indicateur sur toute la fenêtre à chaque `on_bar` (mesuré 92-130 ns/bougie) : `tv4 bench` affiche le coût moyen par `on_bar` pour rendre la dérive visible, et la réponse est le précalcul déclaratif via `IndicatorSpec`.

Comparaison honnête : pour un run unitaire, Python/Polars bien écrit tient aussi sous la seconde (indicateurs) ou en 1-5 s (boucle). Le gain Rust (×20-50) ne compte que sur grilles/walk-forward, jalon 1.5-2. On paie donc l'apprentissage avant d'encaisser le bénéfice : c'est le pari assumé en §13.

## 9. Ce que Rust change par rapport à la version Python

| Point | Version Python/Polars (03 précédent) | Version Rust (ce document) | Conséquence |
|---|---|---|---|
| Langage du moteur | Python 3.12, Polars, NumPy, Pydantic, Typer | Rust 1.97, SoA `Vec<f64>`, serde, clap | Un binaire statique, pas d'interpréteur ; apprentissage réel (ownership, traits, une lifetime) |
| Indicateurs | Expressions Polars vectorisées, cache disque | Structs incrémentales + batch = fold, cache mémoire par campagne | Même code en backtest et en paper (principe 1) ; SMA 1 M points en 6 ms, pas de cache disque nécessaire ; l'incrémental est même plus rapide que le batch (65 vs 114 ms) |
| Boucle | Python par bougie, 1-5 µs | 10-14 ns/bougie mesurés | Grilles et walk-forward in-process avec rayon, 1 000 cellules en secondes (stratégie simple) à dizaines de secondes |
| Stratégies | Classes Python, `Protocol` | Types Rust, trait `Strategy`, `Box<dyn>` | Recompilation 2,6-3,4 s en release par changement de stratégie (mesuré, mono-crate petite ; 5-8 s si l'on touche `engine` ou le trait, croît avec la taille : première scission = `strategy` quand l'incrémental dépasse 10 s) ; params en TOML pour ne pas recompiler sur un réglage |
| Stockage marché jalon 1 | PostgreSQL + Timescale + snapshots Parquet | Parquet seul + manifestes, Postgres au jalon 2 | Un serveur de moins à opérer pendant l'apprentissage ; déviation du défaut, actée le 13/10 (21:47), §13 |
| Orchestration jalon 1 | Temporal (hors jalon 1 déjà) | cron + commandes idempotentes | Identique en pratique ; Temporal revient au jalon 2, avec un SDK Rust officiel désormais disponible (`temporalio-sdk` 1.1.0) |
| Scripting / FFI | Rust envisagé en extension PyO3 ciblée | Aucun Python dans le moteur ; PyO3 écarté sur mesures | Deux langages dans la boucle était le schéma V3 |
| Déterminisme | Dépend de NumPy/Polars (BLAS, threads) | `f64` identique à l'octet pour un même binaire **sous les règles d'ordre de §7.4** ; `rustc`, `target_cpu`, hash du binaire dans le manifeste | Test CI de double run à l'octet réaliste, à condition de proscrire rayon `sum` et `HashMap` itérée |
| Qualité | ruff, mypy strict, pytest | rustfmt, clippy `-D warnings`, `unwrap_used` deny, `disallowed_types/methods`, proptest, insta, criterion | Le compilateur attrape les null et les types ; clippy remplace PHPStan |
| Temps de compilation | 0 s | froid 1-2,5 min, incrémental 1-3,5 s | `cargo check` en boucle, tokio confiné hors jalon 1 ; 1,2-2,4 Go de `target/` |
| Durée du jalon 1 | 3-4 semaines à mi-temps (estimation précédente) | **8-12 semaines à mi-temps avec semaine 0 non comptée** (corrigé, §10-11) | Point de contrôle semaine 3 avec repli explicite |
| Écosystème exchange | ccxt disponible | Aucun client OKX/Bitvavo mature en Rust ; REST/WS/signature écrits à la main au jalon 3 | Coût estimé 2-3× celui de Python pour les connecteurs (à valider) |
| Control plane | Symfony 7 au jalon 2 | **Inchangé** | Les tables PG sont une projection des manifestes |

Ce que Rust **ne change pas** : les biais intrabar (ordre stop/TP, limit rempli au franchissement sans file d'attente), la qualité des stratégies, l'objectif « < 60 s », les gates de sécurité, les frais par exchange. Rust achète de la marge et un seul langage jusqu'au live ; il n'achète pas un PnL.

## 10. Plan d'apprentissage Rust intégré au jalon 1

Pour un senior PHP/Symfony sans Rust, dans l'ordre où les livrables le demandent. Lectures : *The Rust Programming Language* (« Rust Book »), chapitres indiqués ; clippy comme mentor (lire chaque lint).

**Correction (réfutation)** : l'estimation « 6-8 semaines à mi-temps pour le jalon 1 complet » n'était soutenue par aucune donnée. Les seules données chiffrées disponibles (enquête Google 2022-2023 sur 1 000+ ingénieurs à plein temps avec relecteurs Rust, venant de C++/Java/Go : un tiers « aussi productifs » à 2 mois, plus de la moitié à 4 mois ; vision doc Rust 2026 : les seniors « doivent désapprendre » et un profil GC sans ownership est le plus exposé) donnent une productivité partielle jusqu'à 2-4 mois équivalent plein temps. La tranche verticale en semaine 3 reste plausible (reproduite en 223 lignes sans lifetime ni async, 0 `impl<T>`, avec assistance LLM), **à condition qu'elle passe par CSV et non par arrow** (l'API arrow-rs, `Arc<dyn Array>`, `downcast_ref::<Float64Array>()`, est la partie la plus verbeuse et arrive en S4). Le plan ci-dessous ajoute une **semaine 0** non comptée et remplace 6-8 par **8-12 semaines à mi-temps**, en notant que cette estimation suppose l'assistance d'un outil de code qui connaît Rust.

| Semaine / livrable | Concepts à acquérir | Lecture | Transfert et piège depuis PHP |
|---|---|---|---|
| S0 (non comptée) | Rust Book ch. 4, 8, 10, 13 + rustlings | — | Mesurer les heures réellement consommées ; si > 1,5× le prévu, réestimer avant S1 |
| S1 — squelette, `model` | Cargo, workspace, modules et visibilité, `struct`/`enum`/`impl`, `Option`/`Result` et `?`, `match` exhaustif, `#[derive]`, `#[test]` | Book ch. 1-7, 9, 11 | Cargo = Composer, crates = packages, modules = namespaces, clippy = PHPStan max. Piège : chercher les exceptions ; ici `Result` partout |
| S2 — fixture CSV, `indicators`, trait `Strategy`, `sma_cross` | Ownership et emprunts sur `&[f64]`/`&mut Vec<f64>`, `Vec`, itérateurs (`windows`, `zip`, `map`), traits comme interfaces, `Box<dyn Trait>`, serde derive | Book ch. 4, 8, 10, 13 ; chapitre serde | Le code le plus simple de Rust (procédural sur des `Vec`) : c'est là qu'on apprend le borrow checker sur des cas faciles. Piège : `clone()` partout pour calmer le compilateur (acceptable, à nettoyer plus tard) |
| S3 — `sim` market-only, `engine` boucle minimale, **point de contrôle** | `Copy` vs `Clone`, `&mut self`, `Vec` réutilisé, une lifetime (`BarContext<'a>`), `let … else` | Book ch. 10.3 (lifetimes), 15.1 (Box) | Piège : vouloir des objets mutables partagés façon services Symfony ; ici données immuables en paramètres, état mutable confiné dans `Simulator`. Si la lifetime bloque : variante `Arc` + indices (§6), même garantie. Règle qui évite 90 % des combats : la stratégie ne stocke jamais une référence issue du contexte |
| S4 — `store` Parquet, `dataset build`, hashes | API arrow/parquet (builders, `RecordBatch`, `ArrowWriter`, `ParquetRecordBatchReaderBuilder`), `thiserror`/`anyhow`, fichiers et `.tmp` + `rename` + fsync | Docs arrow-rs/parquet **60.0.0** | API verbeuse plutôt que concepts nouveaux ; prévoir une journée perdue ; isoler tout arrow dans `store::parquet` qui rend des `Vec` possédés. Piège : lire une doc d'une autre majeure (`set_max_row_group_size` n'existe plus) |
| S5 — ingestion Kraken complète, `mtf`, continuity | Flux (`Read`/`BufRead`), crates `zip`/`csv`/`sha2`, `Iterator` paresseux, tests proptest | Book ch. 13, docs csv/zip | Piège : charger 2 Go en mémoire au lieu de streamer |
| S6 — `sim` complet (limit, stop, TP, sizing, `IntrabarRule`), tests golden | Enums riches avec données, `match` sur tuples, `insta`, invariants `debug_assert!` | Book ch. 6, 18 | C'est du métier, pas du Rust difficile ; la difficulté est la fidélité des règles, pas le langage |
| S7 — OKX `ureq`, threads std, curseur, `compare-sources` | HTTP synchrone, `std::thread::scope`, `Mutex<Instant>`, JSON `serde_json::Value`, retry, `tracing`, `indicatif` | Docs ureq, Book ch. 16.1-16.3 | Pas d'async : volontaire ; les threads std suffisent. Piège : `unwrap()` sur du JSON externe (clippy `unwrap_used` le bloque) |
| S8 — `report`, `verify`, `bench`, preuve | criterion, profils release vs dev, `cargo flamegraph`, `cargo-llvm-cov` | Docs criterion | Piège classique : mesurer en debug sans `opt-level` (×3-4 mesuré, et ×10 sur une boucle pure) |
| Jalon 1.5 — `grid` | `Arc`, `Send`/`Sync`, `rayon::par_iter`, collecte ordonnée | Book ch. 16 | Se résume à `.par_iter()` sur des données en `Arc`, si `run_cell` est pur |
| Explicitement évité au jalon 1 | async/await, tokio, `unsafe`, génériques avancés et `where`, macros, `Rc<RefCell>`, `Sync` au-delà de ce que rayon impose | — | Tokio arrive au jalon 3 avec le WebSocket, en gardant `sim`/`strategy` synchrones |

Effort estimé : 3 à 4× plus lent qu'en PHP les trois premières semaines, puis 1,5× ; 2-3 semaines à mi-temps pour être productif sur `model`/`indicators`/`sim`, 4-6 semaines pour être à l'aise ; **jalon 1 complet : 8 à 12 semaines à mi-temps**, durée retenue le 13/10 (21:47) (6-8 seulement avec le périmètre réduit décrit en §11, écarté), contre 3-4 en Python. L'écart se résorbe au jalon 2 quand les grilles tournent sans réécriture. Un assistant de code qui connaît Rust réduit sensiblement le temps passé sur les messages du borrow checker ; l'estimation en dépend.

## 11. Jalon 1, détaillé

Ordre choisi pour qu'une **tranche verticale minimale tourne en fin de semaine 3** (parade au risque d'abandon façon V3 : 225 k lignes, 0 trade) ; l'ingestion complète vient après.

1. **S1 — Squelette** : workspace à un membre `tv4` (lib + bin), `rust-toolchain.toml` 1.97, profils (`dev opt-level = 1` + `package."*" opt-level = 3`, `release lto/debug = 1/overflow-checks`), `.cargo/config.toml` avec `target-cpu` fixe, `clippy -D warnings` + `unwrap_used` + `disallowed_types/methods`, `rustfmt`, `config/fees.toml` (okx 20/35 bps, bitvavo_usdc 5/5 bps), `clap` avec les sous-commandes vides, `build.rs` git sha, CI GitHub Actions verte (fmt, clippy, test, llvm-cov, cache cargo) sur un test vide, `control-plane/README.md`, `docs/dataset-hash.md` (spécification `dsh-v1`). `model` : `Candle`, `Timeframe`, `FeeSchedule`, `Slippage`, `MarketEvent`, erreurs, JSON trié + blake3, tests unitaires.
2. **S2 — Tranche verticale, partie 1** : lecteur CSV Kraken minimal via `csv` + serde (fixture 10 000 bougies XBTUSD 1m : XBTUSD est coté sur Kraken depuis le 06/10/2013 et XBTUSDT seulement depuis le 20/12/2019, donc XBTUSDT ne couvre pas le bloc DEV 2018-2022 ; vérifié le 13/10 sur l'API publique `Trades` de Kraken ; l'USD est aussi la meilleure approximation de l'USDC du lieu d'exécution), `indicators` SMA/EMA/RSI/ATR/VWAP incrémentaux + batch = fold, proptest contre une implémentation naïve O(n·k), test de stabilité par préfixe ; trait `Strategy`, `BarContext`, `Signal`, registre, `sma_cross` mono-TF. **Pas d'arrow avant S4.**
3. **S3 — Tranche verticale, partie 2, point de contrôle** : `sim` market-only (portefeuille spot, frais taker, slippage), boucle `engine` mono-TF, `metrics.json` minimal (PnL net, nb trades), `tv4 backtest --fixture`, test allocateur compteur. **Critère : le backtest minimal tourne sur la fixture avec des trades cohérents à la main.** Sinon : revoir le découpage (variante `Arc` sans lifetime, aide extérieure) ou, en dernier recours, bascule sur le plan Python/Polars de la version précédente de ce document (texte hors dépôt, résumé en §9), décision explicite du propriétaire.
4. **S4 — Store** : `parquet.rs` write/read (zstd, un row group par mois via `set_max_row_group_row_count`, `set_created_by("tv4")`, projection, filtre manuel de row groups), `columns.rs` SoA, `.tmp` + `sync_all` + `rename` + fsync répertoire, verrou par partition, `_manifest.json` par fichier avec `content_hash` `dsh-v1` ; `tv4 dataset build` → manifeste + `dataset_hash`, snapshot des mois non clos ; refus de run si un fichier listé a changé de hash ; test aller-retour sur 1 000 bougies, test golden `dataset_hash` en CI, bench de lecture 1 M lignes synthétiques (< 0,3 s à chaud).
5. **S5 — Ingestion Kraken + MTF** : zip (`default-features = false`) → csv en flux, SHA-256 vérifié (`sha2` 0.11), tri/dédup, `continuity.rs` (trous, doublons, non clôturée, OHLC incohérent) avec rapport JSON bloquant, fichiers mensuels (1m, 5m) et annuels (≥ 1h) ; `tv4 ingest kraken` sur le dump complet ; `mtf.rs` avec `closed_count` par timestamps et `just_closed`, oracle Kraken 5m/1h, proptest agrégat ; tests espion (oracle indépendant), perturbation ciblée et mutant (§7.5) ; exploration DuckDB CLI documentée dans le README.
6. **S6 — Simulateur complet** : limit (TTL, `min(limite, open)`), limit de vente (`max(limite, open)`), **plusieurs limits en attente** (`PlaceLimit`/`CancelSlot`, `slot`, réservation du cash et de la quantité à la pose, `max_pending` dans `config_hash`, fills partiels, FIFO par lot), stop (slippage, taker), take-profit (maker), `IntrabarRule`, sizing `PctEquity`/`RiskPct`/`Quote`, arrondi tick/lot, `MoveStop`, `CancelPending`, `forced_close`, MFE/MAE, `signals_rejected`, tests de timing de fill ; invariants `debug_assert!` : `quote_reserved ≤ cash`, `qty_reserved ≤ qty`, jamais négatifs, fill de limit vente dans `[low, high]`, aucun `slot` en double ; tests golden `insta` : un trade gagnant, un stop, stop+TP même bougie (deux règles), limit expiré, gap sous la limite, cash insuffisant, **grille à N niveaux sur une sinusoïde synthétique** (PnL réalisé = `cycles × (espacement − 2 × frais maker)` à l'arrondi près : à 0,20 % maker OKX, un espacement < 0,40 % perd de l'argent, et le test le montre), **tendance baissière synthétique** (la grille finit pleine d'inventaire, `pnl_marked < pnl_net`, `forced_close`) ; bench `pending_20` ; `sma_cross` en 2 TF (tendance H1, entrée M15). Aucune stratégie grid n'est écrite au jalon 1 : seul le simulateur sait tenir plusieurs limits.
7. **S7 — Ingestion OKX** : client `ureq` `history-candles` (300/req au maximum, 100 par défaut, vérifié le 13/10 ; pagination vers le passé avec `after` = `open_time` de la plus ancienne bougie reçue, borne exclusive, réponse triée décroissante, arrêt sur page vide ou `ts < début de fenêtre`, **jamais `before` seul** qui renvoie les plus récentes ; `confirm = "0"` exclue ; toute bougie 1d demandée à OKX l'est en `1Dutc`, qui ouvre à 00:00 UTC comme Kraken, la barre `1D` ouvrant à 16:00 UTC ; les barres 4h tombent déjà sur les bornes UTC), **3 threads `std::thread::scope` + token bucket global à 8 req/s comptant les retries**, retry exponentiel avec jitter sur HTTP 429/5xx/timeout et JSON `code != "0"` (`50011`), pause de 2 s après un 429, pages tamponnées en mémoire et réécriture atomique du mois par lot de 50 pages ou à la frontière de mois, reprise par `max(open_time)` du fichier + retéléchargement de la page chevauchante + dédup ; BTC-USDC 1m ingéré depuis sa cotation du 21/08/2025 (lieu d'exécution, carnet très mince jusqu'en mai 2026) et 1 an de BTC-USDT 1m public gardé comme référence liquide (décision du 13/10 (22:46)) ; `tv4 compare-sources` mesure le biais Kraken/OKX sur le recouvrement (chiffre dans la doc) ; tests avec serveur HTTP mocké (`httpmock` 0.8.3, synchrone, ou `wiremock` 0.6.5, tokio ; ni l'un ni l'autre ne mocke un WebSocket). **Vérifié le 13/10** (doc officielle du site EEE, dépôt `okx/ai-builder-openapi-md` généré le 23/09/2026 depuis la source officielle, et test sur l'API publique) : `limit=300` accepté sur `history-candles` (300 bougies renvoyées ; 1 752 req au lieu de 5 256 pour un an de 1m, ≈ 4 min à 8 req/s), maximum 300 et défaut 100, limite 20 req/2 s par IP, `after` = bougies plus anciennes que `ts`, `before` = plus récentes, `before` seul renvoie les plus récentes, `1Dutc` listée parmi les barres à ouverture UTC+0 (`1D` ouvre à 16:00 UTC, mesuré). **Reste à valider depuis ton poste** : la sémantique de `after` et `before` combinés. Vérifié le 13/10 sur échantillon : les bougies publiques sont identiques sur `eea.okx.com` et `www.okx.com` ; reste à valider que le carnet USD de l'EEE n'est pas un carnet distinct (OKX parle d'un « independent USD EEA orderbook » pour la marge spot), ce que le paper sur le WebSocket EEE mesurera.
8. **S8 — Reports et preuve** : `metrics.json` complet avec verdict (`min_trades` 30), `trades.csv`, `equity.csv` échantillonné, `manifest.json` (`config_hash`, `dataset_hash`, engine avec `target_cpu` et hash du binaire, `result_hash`) ; `tv4 verify`, `tv4 bench` (coût moyen par `on_bar`), bench criterion `engine_2y_1m` archivé avec seuil < 50 ns/bougie ; **preuve** : `tv4 backtest --dataset btcusd_2y --strategy sma_cross --params config/strategies/sma_cross.toml --fees okx` (`btcusd_2y` = 2 ans de XBTUSD 1m tirés du dump Kraken ingéré en S5, comme en S2 ; OKX n'en fournit qu'un an en S7) en **< 5 s en release** sur ton poste (objectif officiel < 60 s ; attendu 0,2-0,5 s), même commande avec `--fees bitvavo_usdc`, deux runs (même job et deux process) → `result_hash` identique vérifié en CI, golden file, couverture publiée, premier résultat métier = biais Kraken/OKX ; **rapport HTML statique** rendu par le squelette Symfony (`control-plane/`, commande console + Twig lisant les fichiers du run, aucune base), sections de la fiche de run de 05-ihm.
9. **Jalon 1.5 (si le temps le permet)** : `tv4 grid` avec rayon, cache précalculé, résultats dans l'ordre de la grille, `campagne.csv`, test 1 thread = 4 threads au bit. Walk-forward au jalon 2.

**Périmètre réduit pour tenir 6-8 semaines**, écarté le 13/10 (21:47) au profit du jalon 1 complet (§13), décrit pour mémoire : ordres market + stop au jalon 1 (limit/TTL et limits multiples en 1 bis), trois indicateurs (SMA/EMA/ATR), OKX en mono-thread sans reprise fine (22-33 min pour 1 an, acceptable puisque le socle multi-années vient des dumps Kraken), couverture mesurée sans seuil.

Hors jalon 1 : WebSocket, paper, ordres, control plane Symfony (hors le squelette de rendu du rapport), Temporal, PostgreSQL, pages web, walk-forward, Bitvavo (connecteur), toute stratégie de type grille (hypothèse de recherche après le connecteur Bitvavo, §15.3), PyO3, et tout l'outillage de recherche du jalon 1.5 (§15.1).

## 12. Risques et parades

| Risque | Parade | Critère de retour / décision |
|---|---|---|
| Courbe d'apprentissage sous-estimée (lifetimes, traits, arrow API) | Semaine 0 non comptée ; ordre S1→S8 qui garde le Rust « ennuyeux » en premier et arrow hors de la tranche verticale ; variante `Arc` sans lifetime (même garantie) ; assistant de code | Point de contrôle S3 ; heures mesurées en S0 ; bascule Python/Polars si rien ne tourne |
| Rust pas nécessaire au jalon 1 (Python tient < 60 s) | Assumé : pari sur grilles/WFO du jalon 2 et sur un seul langage jusqu'au live | Si le projet s'arrête au jalon 1, le pari est perdu ; §13 |
| Monolithe mini-V3 (modules qui se parlent dans tous les sens) | Règle de dépendance écrite (§2) + test CI de `grep` ; scission en crates à 3 000 lignes ou 10 s d'incrémental | Première scission attendue au jalon 2 (`strategy`, `marketdata`) |
| Temps de compilation et disque | tokio hors jalon 1, params en TOML, `cargo check`, `package."*" opt-level = 3`, ≥ 5 Go pour `target/`, cache cargo en CI, `sccache` à mesurer, cargo-chef en Docker | Incrémental > 10 s → scinder |
| SDK qui dérive vers 100-200 ns/bougie (lookup par nom, copie de fenêtre, recalcul dans `on_bar`, `rust_decimal` dans la boucle) | Principe 8 ; bench criterion avec seuil < 50 ns/bougie en CI ; test allocateur compteur | Seuil dépassé → revue du SDK, pas optimisation bas niveau |
| Parquet tronqué par interruption ou coupure machine | `.tmp` + `sync_all` + `rename` + fsync répertoire, `content_hash` vérifié au chargement | — |
| Deux écrivains sur la même partition (Kraken + OKX) | Verrou fichier par (exchange, symbole, TF) | — |
| Mois courant mutable dans un dataset | Snapshot des mois non clos à `dataset build` ; refus de run si hash divergent | — |
| Crash d'ingestion OKX en milieu de mois | Reprise par `max(open_time)` du fichier + dédup ; pages tamponnées, réécriture par lot | — |
| Ingestion OKX trop lente en mono-thread (22-33 min/an) | 3 threads std + bucket partagé ; `limit=300` vérifié le 13/10 | Acceptable en mono-thread si le socle est Kraken |
| Rate limit OKX partagé par IP (WS public, second symbole, autre client) | Bucket global au process ; un seul ingesteur par machine | 429/50011 répétés → réduire à 6 req/s ; machine maison (décision du 13/10 (18:35)) : poste et machine dédiée sortent par la même IP de box, l'ingestion du poste peut gêner le rattrapage REST du live (à mesurer en paper) |
| Catalogue de runs en répertoires JSON | `manifest.json` = format d'échange vers Postgres/Symfony au jalon 2 ; aucun outillage sur le glob | Milliers de runs → import Doctrine, ou `rusqlite` transitoire |
| Timescale écarté au jalon 1 | Parquet seul ; `PgSink` derrière feature `pg-sink` (sqlx 0.9 exige Rust ≥ 1.94, OK) | Besoin de requêtes temps réel sur bougies (jalon 3) |
| Temporal écarté au jalon 1 | Commandes idempotentes + cron ; coût d'abstention nul | Jalon 2 : worker Rust `temporalio-sdk`, Python lanceur, ou Symfony Messenger, à trancher sur le nombre de langages |
| Pas d'async au jalon 1 | Différé, pas évité ; `sim` et `strategy` strictement synchrones pour que seul le connecteur devienne async ; le WS tokio poussera dans un canal (~180 ns/événement mesuré, nul en backtest) | Jalon 3 |
| Monnaie en `f64` | Arrondi tick/lot à chaque fill, frais sur montants arrondis | `rust_decimal` ou entiers en ticks à la frontière exchange, jalon 3 |
| Double run non reproductible malgré rustc | Règles §7.4 vérifiées par clippy (`disallowed_types/methods`), `target-cpu` fixe, libm confinée dans `report`, double run deux process + golden file | Dépendance à dispatch runtime détectée → épingler ou remplacer |
| Look-ahead par indicateur non causal ou `closed_count` faux | Quatre invariants et cinq tests (§7.5), test mutant en CI | — |
| arrow/parquet : une majeure par mois | `=60.0.0` épinglé, montée trimestrielle, jamais en cours de livrable ; test golden `dsh-v1` bloque toute montée qui change le hash | — |
| Fidélité du simulateur (limit optimiste, stop-first pessimiste) | Règles explicites dans `sim_rules`, dans `config_hash` et dans chaque rapport ; `MarketEvent` typé dès le jalon 1 pour accueillir trades/book au jalon 3 sans second modèle | Paper trading jalon 3 = seul correctif ; données trades/book plus tard |
| Bougies absentes non synthétisées | `gaps_policy` dans le manifeste et le hash | `ffill` si une stratégie compte les bougies plutôt que le temps |
| Symboles/devises non alignés (Kraken USD ou USDT vs OKX USDC, décision du 13/10 (22:46)), entité OKX Europe | Dump XBTUSD (coté depuis 2013 ; XBTUSDT seulement depuis le 20/12/2019, vérifié le 13/10) ; biais mesuré en S7 sur BTC-USDT public (référence liquide) et sur BTC-USDC (lieu d'exécution) ; bougies publiques identiques sur `eea.okx.com` et `www.okx.com` (vérifié sur échantillon), carnet USD de l'EEE distinct ou non à mesurer en paper | Backtest « final » toujours sur données OKX du lieu d'exécution (BTC-USDC coté depuis le 21/08/2025 seulement, très mince jusqu'en mai 2026) |
| Panics (index hors bornes, `unwrap`) | `overflow-checks` en release, `unwrap_used` deny, proptest sur les parsers | — |
| Mémoire du cache d'indicateurs en grille | Budget explicite `colonnes × bougies × 8 o` ; lots ; calcul à la volée (+15-25 %) ; sorties réduites dans le worker | > 1 Go → lots |
| Motivation et bus factor d'un solo | Tranche verticale en S3, premier résultat métier en S7 (biais Kraken/OKX) | — |
| Ordre doublé ou perdu sur timeout ou 5xx d'un POST d'ordre (jalon 3) | `clOrdId` déterministe, intention écrite avant l'envoi, `GET` par `clOrdId` avant toute réémission, aucun retry automatique sur un POST d'ordre, réconciliation au démarrage puis à trois cadences (§2) | Format `clOrdId` et rejet de doublon (51016) à valider en demo (G5bis, 04 §6) ; double ordre = PAUSE + critique |
| Position sans stop côté exchange (algo refusé, API dégradée, processus bloqué) | Algo OKX déclencheur → market posé < 60 s et vérifié toutes les 30 à 60 s, réaction graduée et stop logiciel de secours, chien de garde `systemd` et battement (§2) | À valider en demo : l'algo spot reste actif sans connexion du client ; stop absent à 60 s = incident levé et compté (04 §6.1) |
| Machine maison : coupure de box, panne, fuite de la clé | Machine dédiée, stops résidant sur l'exchange, PAUSE à reprise automatique (flux frais, trou rattrapé par REST, réconciliation propre), clé sans droit de retrait et liée à l'IP (06 §3.3), secrets `sops`/`age` ou `LoadCredentialEncrypted=`, IHM par VPN seul (§2, §15.2) | Coupure > 15 min plus d'une fois par mois pendant le paper, ou capital > ≈ 2 000 € → VPS (décision du 13/10 (18:35)) |
| Dette de décision explicite (SQLite, Temporal, crates, `rust_decimal`, tokio) | Listée ici avec son critère de retour | Revue à chaque jalon |

## 13. Décisions du 13/10 (21:47)

Le choix du langage est pris (Rust). Les trois points qui restaient à acter avant le squelette sont tranchés : le propriétaire a retenu les trois recommandations (« les autres questions prend le recommandé »).

1. **Stockage jalon 1 : Parquet seul (sans serveur de base), PostgreSQL au jalon 2 pour les métadonnées, Timescale pour les bougies seulement si besoin au jalon 3.** Retenu. C'est une déviation du défaut « PostgreSQL » de 01-cadrage et du « Timescale + snapshots » de la version précédente, unanime dans les trois propositions, confirmée par mesure (réécriture de mois 30-60 ms, DuckDB ad hoc en dizaines de ms, atomicité du rename vérifiée), désormais actée par le propriétaire. Alternative écartée : Postgres dès le jalon 1 (+1 semaine, `sqlx`, double écriture à tester).
2. **Stratégies en Rust pur, sans scripting ni PyO3**, avec recompilation de 2,6-3,4 s mesurée par changement de stratégie et params en TOML. Retenu. Alternatives mesurées et écartées : PyO3 (≈ 400 ns d'appel + corps Python, ×35 sur grilles), Rhai (3,4-4,1 µs), wasmtime (stratégie à compiler quand même).
3. **Jalon 1 complet en 8-12 semaines à mi-temps avec semaine 0 et point de contrôle ferme en fin de semaine 3** (backtest minimal sur fixture CSV 10 000 bougies, sans arrow). Retenu ; le périmètre réduit de §11 (6-8 semaines) est écarté. Le calendrier de 04 §7 est recalé sur cette durée. Si le point de contrôle n'est pas atteint, la bascule vers le plan Python/Polars de la version précédente est la sortie prévue, pas un échec.

Orchestration jalon 2 (worker Temporal Rust `temporalio-sdk` vs Python lanceur vs Symfony Messenger) n'est pas bloquante : on la tranche quand le control plane commence, avec un SDK Rust officiel désormais sur la table.

Feu vert du propriétaire sur le code le 14/10 (05:21) (« 4 ok mais utilise mes agents avec les modèles adéquat ») : le squelette S1 démarre ce jour-là, chaque tâche confiée à un agent dont le modèle correspond à la difficulté (`CLAUDE.md`) ; la tranche verticale S2-S3 suit, et le jalon 1.5 (§15.1) avant toute campagne.

## 14. Vérifications effectuées (2026-10-13)

### 14.1 Crates confirmées (crates.io / docs.rs, `cargo info`, compilation locale quand indiqué)

| Crate | Version | Statut |
|---|---|---|
| `clap` | 4.6.7 | confirmée, derive avec sous-commandes |
| `serde` / `serde_json` | 1.0.229 / 1.0.154 | confirmées ; clés triées via `to_value` seulement, pas de `preserve_order`, pas RFC 8785 (testé) |
| `toml` | 1.1.8 | confirmée, `Value::try_into` testé |
| `arrow` / `parquet` | 60.0.0 | confirmées ; `set_max_row_group_row_count` validé (l'ancien `set_max_row_group_size` n'existe plus) ; filtre de row groups manuel |
| `csv` | 1.4.0 | confirmée |
| `zip` | 9.0.2 | confirmée, lecture en flux par `by_name` ; désactiver les features par défaut |
| `sha2` | 0.11.1 | **corrigée** (0.10 → 0.11) |
| `ureq` | 3.4.2 | confirmée, synchrone, sans rate limit ni retry intégrés |
| `chrono` | 0.4.45 | confirmée |
| `blake3` | 1.8.7 | confirmée ; les trois hashes sont du code applicatif |
| `thiserror` / `anyhow` | 2.0.21 / 1.0.104 | confirmées |
| `tracing` / `tracing-subscriber` | 0.1.44 / 0.3.23 | confirmées |
| `indicatif` | 0.18.6 | confirmée |
| `rayon` | 1.12.0 | confirmée ; exige `Sync + Send` sur la fermeture |
| `criterion` / `proptest` / `insta` / `tempfile` | 0.8.2 / 1.12.0 / 1.49.0 / 3.27.0 | confirmées (dev) |
| `sqlx` | 0.9.0 | confirmée, COPY disponible, Rust ≥ 1.94 |
| `rust_decimal` | 1.43.0 | confirmée |
| `tokio` / `tokio-tungstenite` | 1.53.2 / 0.30.0 | confirmées ; backend TLS à activer explicitement |
| `httpmock` / `wiremock` | 0.8.3 / 0.6.5 | confirmées (tests S7) |
| Écartées : `polars` 0.55.2, `duckdb` 1.10506.0, `pyo3` 0.29.3 + `maturin` 1.15.0, `rhai` 1.26.1, `wasmtime` 49.0.2, `libloading` 0.9.0, `abi_stable` 0.11.3 (inactive depuis 2023), `ta` 0.5.0 (2021), `yata` 0.7.0 (2024), `reqwest` 0.13.5, `governor` 0.10.4, `backon` 1.6.0, `rusqlite` 0.40.2, `smallvec` 1.16.3, `mimalloc` 0.1.52, `enum_dispatch` 0.3.13 | | existence et raisons d'écartement confirmées |
| `temporal-sdk` | n'existe pas | **corrigée** : la crate officielle est `temporalio-sdk` 1.1.0 (2026-10-06), SDK applicatif Rust supporté par Temporal |
| Outillage : `cargo-llvm-cov` 0.9.1, `flamegraph` 0.6.14 (nom corrigé), `cargo-nextest` 0.9.148, `cargo-deny` 0.20.2, `sccache` 0.18.0, `cargo-chef` 0.1.78 | | confirmés, compatibles Rust 1.97 |

Non vérifiés, laissés « à valider » : `fd-lock`/`fs4` (verrou fichier), `libm` (pure Rust), `serde_jcs` ; sémantique de `after` et `before` combinés côté OKX. Vérifiés le 13/10 : `limit=300` sur `history-candles` (maximum officiel 300, défaut 100, 300 bougies renvoyées à l'essai), limite 20 req/2 s par IP et barre `1Dutc` (doc officielle du site EEE) ; XBTUSD coté sur Kraken depuis le 06/10/2013 et XBTUSDT depuis le 20/12/2019 seulement (API `Trades` de Kraken), d'où XBTUSD en données de recherche (§5, §11) ; Rust stable courante 1.99.0 (01/10/2026) alors que l'étude et l'épingle `rust-toolchain.toml` sont en 1.97 : relever l'épingle au démarrage de S1 (à valider). Hôte EEA vérifié le 13/10 au soir : REST `eea.okx.com`, WebSocket `wseea.okx.com`, démo `wseeapap.okx.com`, clé du site EEE refusée sur un autre domaine (erreur 50119).

### 14.2 Claims : confirmés, corrigés, réfutés

| Claim | Verdict | Ce qui change dans le document |
|---|---|---|
| Boucle complète 11-12 ns/bougie, optimisations bas niveau inutiles | **Confirmé** (10-14 ns reproduits deux fois ; 6-18 ns selon CPU/stratégie) | Mono-cœur explicité ; 100-300 ns reconnu comme coût d'un SDK « Python-like » ; principe 8 et seuil criterion < 50 ns ajoutés |
| Run mur-à-mur 0,2-0,5 s, debug 10-30× plus lent | **Confirmé / corrigé** (0,20-0,38 s mesurés ; debug ×3-4, pas ×10-30) | Profil dev `package."*" opt-level = 3` ; lecture Parquet 60-150 ms selon codec, 350 ms à froid ; coût du hash ajouté |
| Look-ahead impossible par construction, trois tests suffisent, variante sans lifetime plus faible | **Réfuté** (deux contre-exemples compilés ; proptest naïf aveugle ; variante `Arc` de force égale) | §1.2, §6, §7.5 réécrits : bornage d'index + quatre invariants + cinq tests dont test mutant ; lifetime = choix ergonomique |
| f64 identique à l'octet, double run réaliste | **Réfuté dans sa conclusion** (prémisses vraies ; rayon `sum` et `HashMap` cassent le double run) | §7.4 réécrit avec règles d'ordre, clippy `disallowed_*`, libm confinée, `target-cpu` fixe, NaN canonicalisés |
| Parquet mensuel seul suffit, réécriture en « quelques ms » | **Confirmé / corrigé** (30-60 ms ; mois courant mutable ; un seul écrivain) | §5.1 : fsync, verrou, snapshot des mois non clos, fichiers annuels pour TF ≥ 1h, `created_by`, pages tamponnées |
| Hash logique « garantit » la stabilité entre versions arrow | **Réfuté dans sa force** (hacher les buffers Arrow dépend du `batch_size` ; seul un encodage canonique possédé par le projet tient) | §5.3 : format `dsh-v1` spécifié, en-tête littéral, encodage par valeur, test golden en CI |
| Stratégies Rust moins coûteuses que PyO3/Rhai/wasmtime | **Confirmé / nuancé** (Rhai 3,4-4,1 µs avec état ; GIL contournable, argument décisif = ×35 sur grilles ; 2,6-3,4 s de recompilation mesurés) | §0, §3.2, §9, §13 mis à jour |
| ureq + sleep à 8 req/s ⇒ 1 an en 11 min | **Réfuté** (mono-thread borné par le RTT ≈ 250 ms : 22-33 min ; 3 threads + bucket = 7,95 req/s mesurés) | §2, §3, §8, §11 S7 : threads std + token bucket global, retry sur 50011, pagination précisée, points OKX à valider |
| Grille rayon ×3,65, 1 000 cellules en 3-4 s, `Send` suffit | **Réfuté dans sa généralité** (×2,2-4,0 ; 1,5-5 s pour `sma_cross` seulement ; mémoire du cache ; `Send` ni nécessaire ni suffisant) | §6, §7.6, §8 : `run_cell` pur, cache précalculé immuable, budget mémoire, sorties réduites, test 1 = 4 threads |
| Jalon 1 en 6-8 semaines à mi-temps, tranche verticale S3 | **Réfuté pour le calendrier complet** (tranche S3 plausible via CSV ; builds confirmés ; jalon complet non soutenu) | §9, §10, §11, §13 : semaine 0, 8-12 semaines, ou périmètre réduit pour 6-8 |
| Indicateurs incrémentaux + même simulateur en paper, coût nul | **Confirmé** (incrémental 65 ms vs batch 114 ms ; canal mpsc ~180 ns/événement, nul en backtest) | §1.1 : batch = fold de `update`, `MarketEvent` typé dès le jalon 1, horloge par événements, buffers bornés en live |
---

## 15. Raccordement au protocole de recherche (04) et à l'IHM (05)

Le panel d'architecture a tourné en parallèle du protocole de recherche. Cette section aligne le moteur sur les quinze exigences de [04-protocole-recherche.md](04-protocole-recherche.md) §9 et sur les besoins de control plane de [05-ihm.md](05-ihm.md) §5. Chaque écart est tranché ici, pas renvoyé.

### 15.1 Exigences du protocole → modules et jalons

| Exigence (04 §9) | Réponse dans ce document | Écart et décision | Jalon |
|---|---|---|---|
| 1. Batch en mémoire, N configs × M graines dans le même processus | `Arc<Dataset>` chargé une fois, cache d'indicateurs immuable, `run_cell` pur, rayon par cellule (§7.6) | 12 paires × 8,5 ans × 1m en SoA `f64` à 6 colonnes ≈ 2,6 Go en RAM : tenable sur 15 Go, charger seulement les colonnes utiles ; `f32` refusé (prix BTC à 6 chiffres + tick) | 1.5 |
| 2. Reproductibilité, trois temps mesurés dans le manifeste | `config_hash`, `dataset_hash`, `result_hash`, `tv4 verify` (§5.3, §7.4) | Ajouter au manifeste `timings = { load_ms, run_ms, cli_cold_ms }` | 1 |
| 3. Journal de trades complet | `Trade` de §7.2 | Étendre : horodatages signal / ordre / fill / sortie, type de sortie (TP, SL, trailing, **time-stop**, `roll` (F3 : sortie suivie d'une réentrée au même prix sans frais), fin de fenêtre, rejet cap / cash / notional, sortie forcée par trou de cotation), régime à l'entrée, fraction de temps en position | 1 |
| 4. Entonnoir sur chaque run | `signals_rejected` | Compteurs évaluations → signaux bruts → après filtre → ordres → fills → trades, avec motifs, dans `metrics.json` | 1 |
| 5. Profils de coûts par run, règles de fill du protocole | `FeeSchedule` + slippage scalaire, `IntrabarRule` (§7.2) | **Écart de modèle.** `CostProfile` nommé = { grille (`okx_regular`, `bitvavo_usdc` ; `okx_xperps` écarté par décision du 13/10 (18:35) : spot seul, pas de compte dérivés, grille OKX 0,20/0,35 % définitive), spreads par paire, slippage par paire, slippage de stop en distribution + composante de stress }. Les trois profils (baseline, adverse, maker) sont **trois runs** partageant dataset et indicateurs (à 12 ns/bougie c'est gratuit), jamais un run « re-tarifé » : fills et sizing dépendent du coût. Les règles de fill de 04 §3.1 remplacent celles de §7.2 là où elles diffèrent : ordre actif à l'open de la 1m suivante + 1 min (`activation_delay_bars = 1`), limit rempli seulement si une 1m **traverse** le niveau d'un tick (pas au touch), stop rempli au pire de (niveau, open de la 1m qui le traverse) puis slippage tiré de la distribution, stop et TP dans la même 1m = stop, TP touché dans la bougie de fill ignoré, volume 0 = aucun fill, notional > 0,5 % du volume 1m = rejet compté. Tout dans `sim_rules`, donc dans `config_hash` | 1 (règles), 1.5 (distribution de stress calibrée sur les 1m Kraken) |
| 6. Portefeuille spot multi-paires, cash partagé, ≤ N positions, priorité déterministe | §6-7 sont **mono-symbole** | **Écart principal.** Au jalon 1.5, `engine` passe en multi-symboles : un `Simulator` partagé (cash, ≤ N positions, priorité horodatage du signal puis rang de liquidité 90 j, jamais l'ordre alphabétique (04 §6.2)), bougies des paires alignées sur une horloge 1m commune (un trou = la paire n'avance pas), `BarContext` porte `symbol`, mode « risque fixe sans capitalisation » pour la recherche et capitalisé pour paper/live, fichier d'instruments OKX hashé (minSz, lotSz, tickSz des paires XXX-USDC, lu sur l'hôte EEA par `GET /api/v5/account/instruments` avec une clé en lecture seule, car l'API publique des instruments n'est pas filtrée par région et liste BTC-USDT « live » même sur `eea.okx.com` ; `upcChg`) et règle d'exécutabilité de 04 §5 G0 (0,5 % × volume 1m médian ≥ notional, puis minSz), rejet compté par paire avec le motif : à 300 € de capital (décision du 13/10) les lignes font ≈ 17-75 € (3 € de risque / stop de ≈ 18 % à 4 %, 04 §3.1 point 4), donc la règle du volume sort les mid-caps de l'univers live (carnets USDC, volumes quotidiens moyens du 29/09 au 12/10/2026 : seuls BTC et ETH passent pour une ligne de 35 € en 1d, BTC seul pour une ligne de 75 € en 4h maker ; SOL, XRP et DOGE échouent ; à remesurer sur les médianes 1m avant de figer l'univers) et le test « ≥ 95 % des trades exécutables » de 04 §5 G1 doit tourner dès le jalon 1.5 ; l'univers live est fixé une fois à ce notional du palier 100 % et reste le même en paper et aux paliers 20 et 50 % (décision du 13/10 (18:35), option (b)). Le jalon 1 reste mono-paire (tranche verticale) | 1.5 |
| 7. Anti-look-ahead par construction, test « modifier des données futures ne change rien » | §7.5 : quatre invariants, cinq tests, test mutant | Le test demandé est I4 (perturbation ciblée) | 1 |
| 8. Générateur de contrôles aléatoires (régime égal, apparié, sans filtre) | Absent | Nouveau module `research::controls` : (a) régime égal (mêmes sorties, même filtre, même nombre de trades par paire et par an, N variantes × K graines, distribution du max), (b) apparié par config, (c) sans filtre. RNG seedée (`rand` + `rand_chacha`, à vérifier) passée explicitement, jamais globale (§7.4) ; testé sur cas synthétiques | 1.5 |
| 9. Post-traitements du journal | Absent | Nouveau module `research::stats` : bootstrap par blocs (moyenne, LB90, erreur-type), Monte Carlo (ordre, suppression 20 %, décalage d'une bougie, DD p95, probabilité du DD d'arrêt), régimes (BTC / SMA200 × vol 30 j), concentration, benchmark à exposition égale par paire (informatif depuis le 13/10, publié mais non bloquant), Sharpe de l'excédent, N_eff par clustering, Calmar informatif. En Rust sur `trades.csv`, sortie JSON ; `ln`/`exp` confinés comme dans `report` (§7.4) | 1.5 |
| 10. Runner de campagne | `tv4 grid` (§7.6) | Ajouter plateau (voisins ±25 %), walk-forward ancré sur DEV par re-run, classement par LB90, dégonflement de μ_DEV, rapport au format fixe de 04 §4.5 publié même négatif | 1.5 (grid, plateau), 2 (wfo) |
| 11. Registre et garde-fous CLI | Absent | `tv4` écrit une ligne dans `research/trials.csv` à chaque `config_hash` inconnu ; un dataset porte un champ `block` (DEV, VALIDATION, HOLDOUT, FORWARD) dans son manifeste et `tv4 backtest` **refuse** VALIDATION/HOLDOUT sans fiche gelée (tag git `frozen/H-NNN`) ni claim inscrit ; compteurs dans `research/README.md` ; triplet (N, N_eff, Sharpe de bruit q95) imprimé sur chaque rapport. Au jalon 2a le control plane tient le registre en base et `tv4` reste la source de vérité des hashes | 1.5 |
| 12. Synthétiques de certification | Absent | `tv4 certify` : générateur de stratégies nulles et à Sharpe injecté (2,0 / 1,2 / 1,0), portes G1-G2 rejouées, rapport de puissance ; rejoué à chaque modification du moteur ou des portes (G0bis) | 1.5, avant la première campagne |
| 13. Données : bougies absentes, qualité, survivance, univers, biais, blocs | Lecteur Kraken, `continuity.rs`, `compare-sources` (§11) ; `gaps_policy = skip` par défaut (§5.1) | **Écart tranché** : le protocole veut les bougies absentes **reconstruites à volume 0 et marquées** (aucun fill possible dedans). Troisième valeur `gaps_policy = zero_volume_marked` (close précédent, `synthetic = true`, `volume = 0`), défaut des datasets de recherche ; `skip` et `ffill` restent disponibles ; la politique est dans le manifeste donc dans `dataset_hash`. Ajouter : rapport de qualité par (paire, TF, trimestre) avec date de première bougie et trous de cotation, test de survivance (paires attendues absentes du zip), `tv4 universe build` par trimestre (test 5 / 8 / 9 paires en 2018), biais Kraken/OKX sur clôtures 4h **et** mèches 1m, quatre datasets figés DEV / VALIDATION / HOLDOUT / FORWARD avec leur `dataset_hash` | 1 (lecteur, qualité, compare-sources), 1.5 (univers, blocs, survivance) |
| 14. Parité backtest / paper / live | Principe 1, module `execution` | Rejeu hebdomadaire = `tv4 backtest` sur la semaine OKX + diff signal par signal ; test apparié des coûts dans `research::stats` | 3 |
| 15. Exécution live | Gates V3 et règles du module `execution` (§2) | Règles d'arrêt de 04 §6 exécutées par le code (PAUSE, ARRÊT, CASH), CUSUM mensuel ; pause journalière à −2 % avec reprise automatique au jour UTC suivant (décision du 13/10 (18:35)) ; seuils en USDC sur le `capital alloué` du palier en cours (60, 150 puis 300 €, option (b)) ; réconciliation au démarrage puis à trois cadences au lieu d'une seule quotidienne (§2) ; G5bis avant le premier ordre réel (demo OKX ≥ 2 semaines et game day, scénarios joués d'abord contre l'OKX mocké de S7, 04 §6) | 3 |

Conséquence sur le calendrier : entre le jalon 1 (§11) et la première campagne s'insère un **jalon 1.5 « outillage de recherche »** (multi-paires, profils de coûts, contrôles, stats, registre, certification, univers, blocs), estimé à 4 à 6 semaines à mi-temps (à valider, même réserve qu'en §10). C'est la phase A du calendrier de 04 §7 ; aucune campagne ne démarre avant `tv4 certify` vert.

### 15.2 Besoins de l'IHM → control plane

| Besoin (05 §5) | Réponse | Jalon |
|---|---|---|
| Rapport HTML statique du jalon 1 | `control-plane/` : squelette Symfony minimal, commande console `tv4:report <run_dir>` qui lit `metrics.json`, `trades.csv` et les sorties de `research::stats`, et rend des templates Twig réutilisés par la fiche de run ; aucune base de données au jalon 1 | 1 |
| API de lecture : runs, trades, plis, distributions, événements, couverture | Tables PostgreSQL = projection des manifestes (§5.4) + import des CSV ; API Platform en lecture ; progression d'un run écrite par `tv4` dans `runs/<id>/progress.json` et importée (Turbo Frame toutes les 5 à 10 s) | 2a, 2b |
| Manifeste gelé à l'envoi, type exploratoire / confirmatoire, compteurs d'essais | Le control plane écrit la fiche `H-NNN.md`, le tag git et la ligne de `trials.csv` **avant** de lancer `tv4` ; `tv4` reste la source de vérité des hashes et refuse ce qui n'est pas gelé (§15.1, ligne 11) | 2a |
| « Déverrouiller le holdout », quotas (3 par trimestre, 1 par trimestre et 3 à vie) | Compteurs lus dans `research/README.md` puis en base ; l'action crée la fiche confirmatoire et le claim, puis lance `tv4` sur le dataset HOLDOUT | 2a |
| Commandes idempotentes avec accusé horodaté | Endpoints POST avec clé d'idempotence, réponse { état, horodatage serveur UTC, identifiant de journal } ; couper, fermer, réarmer passent par un fichier de commande lu par `execution` (le kill switch durable reste le fichier `var/kill`), jamais par un appel direct au moteur | 3 |
| Flux d'événements : positions et fills, santé, journal | `execution` écrit dans PostgreSQL ; Mercure + Turbo Streams côté Symfony pour ces trois flux seulement ; Redis pour l'état courant | 3 |
| Identité, 2FA, CSRF, journal d'audit | Accès distant par VPN seul (WireGuard, un seul port UDP ouvert, aucune exposition HTTP ; décision du 13/10 (18:35), revient sur 05 §7.1 et §2.10) ; prérequis à vérifier depuis la box : IP publique fixe ou DynDNS fiable, pas de CGNAT, sinon tunnel sortant (à valider) ; utilisateur unique (décision du 2026-10-13), login Symfony + TOTP conservés à l'intérieur du VPN (défense en profondeur), CSRF sur les commandes, chaque commande journalisée avec son origine (web, fichier kill) ; aucune notification sortante | 2a (login), 3 (commandes) |
| Vérification périodique des stops côté exchange, réconciliation exchange ↔ base | Tâches périodiques de `execution` à trois cadences (algo stop de chaque position toutes les 30 à 60 s ; ordres et positions toutes les 5 min ; fills et soldes toutes les heures, complète chaque jour ; §2), résultats dans les mêmes tables ; orphelin créé par nous (notre schéma, sans position) = annulé, avertissement résolu automatiquement ; tout autre écart = alerte critique dans le Journal et entrées suspendues | 3 |

### 15.3 Décision du 2026-10-13 : ordres limit multiples

Décision du propriétaire : le grid bot n'est pas au jalon 1, mais l'interface `Strategy` et le simulateur permettent dès la conception qu'une stratégie tienne **plusieurs ordres limit en attente en même temps**, pour qu'un grid avec filtre de tendance puisse être testé plus tard comme hypothèse de recherche ordinaire (même protocole 04 : coûts nets, contrôles aléatoires, holdout, benchmark à exposition égale), sur Bitvavo USDC à 0,05 % plutôt que sur OKX à 0,20 %, donc après le connecteur Bitvavo. Texte d'origine : note de proposition du 13/10, intégrée dans cette section ; lacunes du marché : [06-lacunes-bots-marche.md](06-lacunes-bots-marche.md).

| Ce qui est tranché maintenant (pour ne pas refondre plus tard) | Où | Jalon |
|---|---|---|
| `Signal::PlaceLimit { side, price, size, ttl_bars, slot, tag }` et `Signal::CancelSlot { slot }` ; `PendingOrder` porte `side` et `slot` ; `BarContext` expose `qty_free` et `pending_slot()` | §6 | 1 (S2 pour les types, S6 pour les fills) |
| Fill d'une limit de vente symétrique de l'achat ; cash et quantité **réservés à la pose** ; fills partiels, `avg_price` recalculé, journal FIFO par lot ; `max_pending` (défaut 64) dans `SimRules` donc dans `config_hash` ; minimum de notional par paire | §7.2 | 1 (S6), minimums au 1.5 avec le fichier d'instruments |
| Rien de grid-spécifique dans `engine`, `sim` ou `strategy` : pas de type « grille », pas de paramètre d'espacement dans le moteur | §6 | — |
| Coût : estimation +20 à +60 ns/bougie pour 20-40 limits, nul sans `PlaceLimit` ; bench `pending_20` | §8 | 1 (S6) |

Ce qui reste à écrire **dans la fiche `H-NNN` du grid**, pas ici : la règle de sortie de fourchette (le stop de la position agrégée via `MoveStop`, qui maintient l'invariant « aucune position sans stop » du live), le filtre de tendance, l'espacement minimal (> 2 × frais maker + glissement, donc > 0,40 % sur OKX et > 0,10 % sur Bitvavo USDC, à valider sur les frais réels), et le traitement de l'inventaire en tendance baissière. Le simulateur le rendra visible, il ne le décide pas.
