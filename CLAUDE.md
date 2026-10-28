# tradingV4

Plateforme de trading crypto **spot** sur OKX : moteur de backtest, de paper trading et d'exécution en Rust, control plane Symfony. Objectif fixé par le propriétaire : un système qui trade réellement et dégage un PnL net positif, démontré par un protocole de recherche avant tout euro engagé.

## État du dépôt

- Conception terminée le 2026-10-13 : documents 01 à 08 dans `docs/`. Les trois points de `docs/03-architecture.md` §13 sont tranchés (décisions du 13/10, 21:47), ainsi que le seuil d'effectif de G0, maintenu à 300 trades en 1d (13/10, 21:59, `docs/08-strategies-risque-exploitation.md` §A.6). Plus aucun point de conception n'est en suspens. Le 14/10 (05:21), le propriétaire a donné la grille de frais de son compte (« 0,2 », lu comme le maker de la grille 0,20 / 0,35 %, baseline inchangée), accepté la ligne BTC du palier 20 % au minimum d'ordre et l'univers live du cycle 1 réduit à BTC et ETH (`docs/04-protocole-recherche.md` §10, décisions 6 et 7). Restent à lire sur le compte OKX Europe la liste des paires USDC autorisées avec leurs minimums (`GET /api/v5/account/instruments`) et le taker (`GET /api/v5/account/trade-fee`), et à remesurer l'univers live sur les médianes 1m des carnets USDC avant G0.
- **Feu vert du propriétaire sur le code le 14/10 (05:21).** Le jalon 1 avance étape par étape, une issue et une PR par étape, S1 à S8 (`docs/03-architecture.md` §11).

## Documents, à lire avant toute modification

| Fichier | Contenu |
|---|---|
| `docs/01-cadrage.md` | Objectif, ce qu'on reprend de tradingV3 (les idées, pas le code), périmètre du jalon 1 |
| `docs/02-matrice-exchanges.md` | Choix de l'exchange : Binance exclu pour un résident français, Bybit EU étudié le 13/10 et non retenu, OKX spot retenu |
| `docs/03-architecture.md` | Architecture du moteur Rust, modules, performance, jalon 1 détaillé (S1 à S8) |
| `docs/04-protocole-recherche.md` | Protocole de recherche d'edge : blocs DEV / VALIDATION / HOLDOUT, portes G0 à G7, paper et live |
| `docs/05-ihm.md` | IHM du control plane Symfony |
| `docs/06-lacunes-bots-marche.md` | Ce qui manque aux bots du marché : journal fiscal, contraintes d'exchange, sécurité de la clé |
| `docs/07-sources-de-rentabilite.md` | D'où vient le PnL des bots, et ce qui survit à nos frais |
| `docs/08-strategies-risque-exploitation.md` | Familles de stratégies, risque et capital à 300 €, mode opératoire, décisions du 13/10 |

## Décisions du propriétaire (ne se rediscutent pas sans lui)

- Crypto **spot uniquement**, aucun compte ni produit dérivé ; exécution OKX Europe spot en paires USDC, décision du 13/10 (22:46), l'USDT n'étant pas échangeable pour un résident de l'EEE ; frais 0,20 % maker / 0,35 % taker, maker confirmé sur le compte le 14/10 (un avis OKX du 11/09/2026 annonçant 0,10 % / 0,20 % pour le compte spot seul n'y apparaît pas).
- Jalon 1 : Parquet seul (PostgreSQL au jalon 2), stratégies en Rust pur sans scripting, périmètre complet en 8-12 semaines à mi-temps (`docs/03-architecture.md` §13).
- Moteur en Rust ; control plane Symfony au jalon 2, précédé au jalon 1 d'un squelette Symfony qui rend le rapport HTML statique (`docs/03-architecture.md` §11, S8).
- Données : dumps OHLCVT Kraken et `history-candles` OKX ; paper trading sur le WebSocket public de l'EEE (`wseea.okx.com`).
- Capital de départ 300 € au maximum ; pilote en échelle 20 % → 50 % → 100 % ; verdict sur 12 mois à 100 %, avant impôt, contre le Livret A.
- Recherche limitée au 1d et au 4h sur OKX (1h en maker seulement, hors cycle 1) ; F1 (cassure Donchian 55 + filtre BTC > SMA200) en tête ; F3 documentée mais rejetée par G0 (seuil de 300 trades en 1d maintenu), repli effectif F4.
- Un seul utilisateur, aucune notification sortante ; IHM accessible par VPN seul ; machine dédiée à la maison.
- Pilote : au palier 20 %, ligne BTC = max(risque / stop, minimum d'ordre BTC-USDC) avec un risque effectif plafonné à 1,5 % du capital alloué ; univers live du cycle 1 : BTC et ETH en 1d, BTC seul en 4h maker (décision du 14/10 (05:21)).
- Qualité des données : un trimestre est exclu pour une (paire, TF de décision) au-delà de 1 % de bougies absentes ; le 1m qui sert aux fills n'exclut rien, une minute absente y vaut « pas de fill » (décision du 15/10 (12:34), `docs/04-protocole-recherche.md` §2.5 et §10 décision 8).
- Simulation : fills de limit pessimistes, règles `s6.1` (une limit marketable à son arrivée part en taker à l'open, une limit au repos est remplie à son prix exact en maker, même à travers un gap ; décision du 23/10, `docs/04-protocole-recherche.md` §3.1 et §10 décision 9).
- S8 : les 13 points de `docs/specs/s8.md` §17 sont validés en bloc (décision du 28/10 (12:05 UTC), carte « Tout valider »). Ils couvrent :
  - Symfony 7.4 LTS sur PHP 8.3 pour le squelette ;
  - fenêtre de `btcusd_2y` dans DEV (2021-01-01 → 2023-01-01) ;
  - porte criterion bloquante et couverture ≥ 94 % ;
  - R hors frais, et trop peu de trades jugé avant l'expectancy négative ;
  - `return_annualized` gardé (20 ULP) ;
  - rejets comptés dans l'entonnoir ;
  - régime à l'entrée de `trades.csv` reporté au jalon 1.5.
- S9 : les 12 points de `docs/specs/s9.md` §20 autres que le capital (point suivant) sont validés en bloc (décision du 28/10 (17:18 UTC), carte « Tout valider »). Ils couvrent :
  - `upcChg` stocké et haché sans être appliqué, et `tv4 instruments fetch` qui refuse toute clé dont `perm` n'est pas en lecture seule ;
  - rang de liquidité 90 j calculé sur les données du run ;
  - régime exposé en série (clôtures 1d closes, SMA200) plus `bull()`, une stratégie par paire ;
  - trou de cotation à T = 7 jours, grille = union des minutes présentes, démarrage commun des paires ;
  - `min_order_bump` en S9, désactivé par défaut ;
  - G0 au notional tiré des entrées du run, plafonné à 300 €, avec les minutes absentes à 0 dans la médiane de volume.
- Mode portefeuille (S9) : capital initial hors du `config_hash`. Un même hash doit donner les mêmes trades à 300 € et à 3 000 € (04 §4.2). Le capital est écrit dans `metrics.json` et dans le manifeste, et `tv4 verify` refuse deux runs de capitaux différents (décision du 28/10 (13:52 UTC), carte « Hors du hash »).

## Code (moteur Rust `tv4`)

- Workspace Cargo à un membre, `tv4/` (lib + binaire `tv4`), Rust 1.97.0 épinglé par `rust-toolchain.toml` ; arborescence et règle de dépendance entre modules : `docs/03-architecture.md` §2 et §4 (vérifiée par `tv4/tests/architecture.rs`).
- Avant tout commit, depuis la racine : `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets --all-features -- -D warnings`, `cargo test --locked --all-targets`. La CI (`.github/workflows/ci.yml`) lance les mêmes commandes, plus la couverture `cargo llvm-cov`.
- `Cargo.lock` est commité. Une crate de `docs/03-architecture.md` §3.1 pas encore utilisée est déclarée avec `=` dans `[workspace.dependencies]` pour garder la version vérifiée jusqu'à son premier usage.
- Identifiants en anglais ; commentaires, messages d'erreur et documentation en français. `HashMap`/`HashSet` interdits par clippy (ordre non déterministe) ; `unwrap()` interdit dans la lib.
- Format du hash des données : `docs/dataset-hash.md` (`dsh-v1`), qui fait foi pour l'implémentation de S4 et pour un recalcul en PHP au jalon 2.
- Spécification de S8 (rapports `metrics.json`, `trades.csv`, `equity.csv`, `manifest.json`, `tv4 verify`, `tv4 bench`, squelette Symfony `control-plane/`) : `docs/specs/s8.md`, qui fait foi pour le détail de S8 ; les 13 points soumis au propriétaire (§17) sont validés depuis le 28/10.
- Spécification de S9 (portefeuille multi-paires, `tv4 instruments fetch`, rapports `tv4-metrics-s9` et `tv4-manifest-s9`, exécutabilité G0 et G1, trou de cotation, régime BTC/SMA200) : `docs/specs/s9.md`, qui fait foi pour le détail de S9 ; ses points soumis au propriétaire (§20) sont tous validés depuis le 28/10.

## Règles de travail

- Documentation, commits et échanges en français.
- Jamais de secret dans le dépôt (clés API, fichiers `.env`) ; la clé OKX n'a jamais le droit de retrait, et le moteur passe `tdMode = cash` sans jamais activer l'emprunt (OKX Europe propose la marge spot jusqu'à 10×).
- Toute hypothèse de recherche passe par une fiche pré-enregistrée et commitée avant tout run sur DEV (`docs/04-protocole-recherche.md` §4.3), puis par les portes G0 à G7.
- Un résultat de backtest n'est valide qu'avec son `config_hash`, son `dataset_hash` et le commit du moteur ; en mode portefeuille (S9), le capital initial, sorti du `config_hash` comme le veut 04 §4.2, s'y ajoute (décision du 28/10 (13:52 UTC)).
- Pas de score composite ni d'empilement multi-timeframe à score : abandonnés après tradingV3.

## Travail par agents (décision du 14/10)

Demande du propriétaire : « utilise mes agents avec les modèles adéquat ». Le thread qui pilote une étape délègue le travail à des agents et garde la coordination, les arbitrages et la revue des livrables. Modèle choisi selon la tâche, sur le modèle de la politique de tradingV3 :

| Travail | Modèle | Effort |
|---|---|---|
| Inventaire, lecture ciblée, contrôle mécanique des critères d'acceptation | Haiku | bas |
| Implémentation bornée d'une étape (code et tests), application des retours de revue | Sonnet | élevé |
| Spécification (formats, hashes), invariants du simulateur, revue consolidée d'une PR | Opus | élevé |

- Un seul cycle de revue par PR : revue Opus, corrections Sonnet, contrôle Haiku, puis vérifications ciblées (règle reprise de tradingV3, instruction du 08/10).
- On passe à un modèle supérieur seulement sur une difficulté précise et documentée.
- Les agents ne commitent ni ne poussent : le thread pilote commite, pousse, ouvre la PR et la mène au vert.
