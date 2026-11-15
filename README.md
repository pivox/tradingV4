# tradingV4

Plateforme de trading crypto spot sur OKX : un moteur Rust de backtest, de paper trading et d'exécution, et un control plane Symfony. La conception complète, l'architecture et le protocole de recherche sont dans `docs/` (point d'entrée : `docs/01-cadrage.md`).

Les décisions du propriétaire et les règles de travail du dépôt sont dans `CLAUDE.md`. Le moteur se construit et se teste depuis la racine avec `cargo build --release` et `cargo test --locked --all-targets`.

## Backtest, preuve et rapport (S8)

Depuis la racine du dépôt, sur la fixture de 10 000 bougies (le dataset `btcusd_2y` de la preuve se construit sur le poste du propriétaire, `docs/03-architecture.md` §11 S8) :

```sh
# Run : metrics.json, trades.csv, equity.csv, stats-inputs.json puis manifest.json (écrit en dernier) dans --out
# (défaut : <root>/runs/<run_id>, ou <root>/runs/<profiles_id> avec --costs, mode s10 de
# docs/specs/s10.md). Dans un dossier existant, l'ancien manifest.json, l'ancien report.html et
# un ancien profiles.json sont supprimés avant l'écriture : un dossier sans manifeste n'est pas
# un run complet
cargo run --release --bin tv4 -- backtest --fixture fixtures/kraken_xbtusd_1m.csv --strategy sma_cross \
  --params config/strategies/sma_cross.toml --fees okx --out runs/exemple-a

# Second run identique, dans un autre dossier, pour la comparaison qui suit
cargo run --release --bin tv4 -- backtest --fixture fixtures/kraken_xbtusd_1m.csv --strategy sma_cross \
  --params config/strategies/sma_cross.toml --fees okx --out runs/exemple-b

# Comparaison à l'octet de deux runs : code 0 identiques, 3 différents, 1 erreur ou run incohérent
cargo run --release --bin tv4 -- verify runs/exemple-a runs/exemple-b

# Coût de la boucle par 1m et coût moyen par appel de on_bar (mêmes entrées que backtest, sans artefact de run ; registre mis à jour)
cargo run --release --bin tv4 -- bench --fixture fixtures/kraken_xbtusd_1m.csv --strategy sma_cross \
  --params config/strategies/sma_cross.toml --repeat 5

# Rapport HTML statique d'un run (control plane Symfony, PHP 8.3)
(cd control-plane && composer install && php bin/console tv4:report ../runs/exemple-a --output ../runs/exemple-a/report.html)
```

Le rapport s'écrit par défaut dans `<run_dir>/report.html`. Codes de sortie de `tv4:report` : 0 rendu écrit, 1 run invalide ou écriture impossible, 2 erreur d'usage (argument manquant, option ou commande inconnue). L'environnement se règle par `--env`/`-e` (sinon `APP_ENV`, sinon `prod`) ; le debug est actif par défaut, `APP_DEBUG=0` ou `--no-debug` le coupe, et dans ce cas il faut vider `var/cache/` du control plane après un `git pull` (le cache Twig compilé est désactivé, mais pas le conteneur Symfony). Détail : `control-plane/README.md`.

`tv4 verify` contrôle la cohérence des fichiers et, lorsqu’il existe, le contexte de provenance `governance`. Les compteurs historiques d’admission ne participent pas à la comparaison des simulations ; rejouer une configuration après d’autres essais reste comparable. Les timings restent informatifs.

Contrôles de reproductibilité en CI (`.github/workflows/ci.yml`) : le backtest de fumée tourne deux fois en deux process puis `tv4 verify` ; les deux goldens de run `tv4/tests/golden_runs/sma_cross_15m_okx/` (fumée, 2 trades) et `tv4/tests/golden_runs/sma_cross_1m_court_okx/` (49 trades, verdict, distribution) détectent la dérive entre commits, le bloc `approx` à `approx::ULP_TOLERANCE` ULP (20) et le reste à l'octet (mise à jour des deux : `TV4_GOLDEN_UPDATE=1 cargo test --test golden_run`, refusée si `CI` est définie, puis relecture du diff) ; le bench criterion `engine_2y_1m` (`cargo bench --locked --bench engine -- engine_2y_1m`) est archivé et sa borne basse est limitée à 50 ns par 1m par `.github/scripts/criterion-gate.sh`. La spécification de S8 est `docs/specs/s8.md` ; les points qu'elle laisse à la validation du propriétaire sont en §17.

## Registre et autorisations de recherche (S12)

Chaque `backtest` et chaque invocation de `bench` inscrit ses configurations dans
`research/trials.csv`, avant le moteur. N compte les configurations distinctes :
rejouer un hash ajoute une lecture, pas un essai. Les trois profils de coûts
comptent trois configurations ; les deux lignes ne les doublent pas.
Le verrou permanent est sous `state/locks/`. `research/README.md` est la projection
du journal ; ne jamais éditer le CSV pour récupérer une lecture ou un claim.

```sh
cargo run --release --bin tv4 -- research status
# --root concerne les données ; --research-root le checkout Git de l'étude.
cargo run --release --bin tv4 -- research status --research-root /chemin/etude
```

Une fixture ou un ancien dataset sans bloc peut être joué sans fiche : son origine
reste `unregistered_hypothesis`. Tout dataset de recherche exige une fiche
`research/H-NNN.md` remplie et commitée, identique à HEAD. Partir du
[modèle](research/H-000-modele.md), qui n'est pas exécutable, puis donner
`--hypothesis H-NNN` à `backtest`. Changer `--root` ou `--out` ne remet aucun
compteur à zéro. Pour une démonstration isolée, utiliser un autre checkout Git
avec son `--research-root`, comme les tests et les fumées de CI.

VALIDATION et HOLDOUT exigent en plus un candidat déjà testé sur DEV, associé
à la même fiche, puis gelé. Le fichier commité `research/frozen/H-NNN.json`
est couvert, avec la fiche, par le tag Git `frozen/H-NNN`. Son contrat est :

```json
{
  "schema": "tv4-frozen-candidate-v1",
  "hypothesis": "H-001",
  "fiche_sha256": "<SHA-256 exact des octets de la fiche, 64 hex minuscules>",
  "config_hashes": ["<config_hash déjà enregistré>"],
  "engine_commit": "<engine.git_sha du manifeste DEV>",
  "engine_binary_hash": "<engine.binary_hash du même binaire>",
  "okx_subset_hash": "<subset_hash du sous-ensemble OKX figé>"
}
```

`config_hashes` contient soit un hash, soit les trois profils exacts. Compiler le
binaire avant le gel et conserver ce binaire ; un moteur ou un sous-ensemble
différent est refusé. L'inscription et l'association des hashes doivent précéder
le gel. Le tag n'est jamais créé implicitement par la CLI.

```sh
# Après avoir commité la fiche et le candidat puis créé frozen/H-NNN,
# invoquer directement le binaire conservé avant le gel :
./target/release/tv4 research claim --hypothesis H-NNN
# Dans la commande backtest de bloc habituelle :
# ... --hypothesis H-NNN --okx-subset chemin/okx_usdc.json
```

Le claim HOLDOUT s'inscrit dans une invocation antérieure à la lecture. Une
réservation consomme le claim et le quota même en cas d'échec ou d'interruption.
VALIDATION autorise trois candidats par trimestre UTC, une lecture par candidat ;
HOLDOUT une lecture par trimestre, trois par génération. Après rotation, l'ancien
HOLDOUT devient VALIDATION et les trimestres complets de FORWARD postérieurs au
gel et à la génération précédente deviennent HOLDOUT. Le bloc effectif figure
dans le rapport. Le benchmark mono-paire refuse tout dataset de recherche.
L'ancien contournement `--unguarded-protected-read` a été retiré.

Le manifeste conserve N, N_eff non calculé, T de sélection DEV (5 ans), les
repères BLP/q90/q95 et les identifiants d'admission, hors hashes et métriques de
simulation. La CLI et le HTML affichent « Essais et lectures ». Ces repères du
Sharpe de l'excédent à exposition égale ne sont pas une porte de validation.
Un ancien rapport sans contexte le signale ; un ancien run `unguarded` garde son
avertissement. Détail : [conception S12](docs/specs/s12.md) et
[protocole](docs/04-protocole-recherche.md).

## Statistiques et vrais rejeux décalés (S13)

Les nouveaux backtests conservent `stats-inputs.json` : prix issus des séries déjà
admises, marquages quotidiens et de réalisation, cash du moteur et timeframe de
décision. Le manifeste référence ce paquet par BLAKE3. Il permet le calcul sans
recharger les bougies, même si elles ne sont plus présentes ensuite.

Après le backtest technique de l'exemple précédent :

```sh
cargo run --release --bin tv4 -- stats runs/exemple-a --seed 42
(cd control-plane && php bin/console tv4:report ../runs/exemple-a)
```

`stats.json` publie bootstrap, Monte Carlo marqué, régimes, concentration,
benchmarks et clustering avec leurs effectifs et limites. La graine est requise
et conservée comme chaîne décimale, sur toute la plage `u64`. Deux appels sur les
mêmes entrées, options et graine produisent les mêmes octets. Les sources, le
registre d'essais et ses compteurs restent intacts. Un dossier de profils ou de
lignes reçoit un `stats.json` par feuille.

Une donnée manquante produit une valeur `null` avec sa raison : absence de
BTC/ETH, chauffe insuffisante, marquage quotidien incomplet, risques R absents
ou corpus incomplet. Un ancien run sans paquet conserve les calculs identifiables
sur son journal. `--inputs <stats-inputs.json>` peut fournir le paquet d'un rejeu
admis dont les trois artefacts de simulation sont strictement identiques ; il ne
déclenche aucune lecture de bougies ni aucun rejeu automatique.

Le test de décalage utilise deux vrais backtests avec le même binaire :

```sh
# Réécrire aussi le run normal avec ce binaire pour fixer le moteur de comparaison.
cargo run --release --bin tv4 -- backtest --fixture fixtures/kraken_xbtusd_1m.csv --strategy sma_cross \
  --params config/strategies/sma_cross.toml --fees okx --out runs/exemple-a
cargo run --release --bin tv4 -- backtest --fixture fixtures/kraken_xbtusd_1m.csv --strategy sma_cross \
  --params config/strategies/sma_cross.toml --fees okx --entry-delay-bars 1 --out runs/exemple-decale
cargo run --release --bin tv4 -- stats runs/exemple-a --seed 42 --delayed-run runs/exemple-decale
```

Une bougie signifie un timeframe de décision supplémentaire pour les seuls achats,
TTL compris. La règle entre dans le `config_hash` : S12 compte cette nouvelle
configuration et le gel d'une configuration normale ne l'autorise pas
implicitement sur un bloc protégé. Le rapport indique les expectancies nettes,
leur ratio et le contexte DEV du rejeu ; aucune valeur ne constitue un verdict G2.
Ces commandes de fixture restent des vérifications techniques.

`--peer-run <run_dir>` répété fournit un corpus comparable ; `--research-root`
choisit le registre dont l'instantané est lu sous verrou. Le rapport distingue
le N historique d'admission, le N courant vérifiable et les M configurations
observées. Les clusters du panel ne deviennent un N_eff global que si le corpus
est complet et vérifié ; les repères de bruit restent calculés sur N déclaré.
La référence à exposition égale est descriptive, le Calmar est informatif à
partir de 36 mois calendaires et la borne mensuelle n'est calculée qu'en DEV.

Conventions et contrats : [conception S13](docs/specs/s13.md).

## Exploration DuckDB

Les bougies sont des fichiers Parquet ordinaires sous `data/candles/<exchange>/<SYMBOLE>/<tf>/`, lisibles par DuckDB sans import. `open_time_ms` est un entier en millisecondes UTC : `epoch_ms(open_time_ms)` le convertit en horodatage UTC sans dépendre du fuseau de la session (contrairement à `to_timestamp`, qui rend un `TIMESTAMP WITH TIME ZONE` affiché dans le fuseau local).

Les requêtes ci-dessous se lancent depuis la racine de travail, par exemple `cd ~/data/tv4` puis `duckdb`. Elles ont été vérifiées avec DuckDB 1.5.6 (module Python, même moteur que la ligne de commande) sur le Parquet que `tv4 ingest csv` produit à partir de `fixtures/kraken_xbtusd_1m.csv`, soit 10 000 bougies de BTC-USD en janvier 2020. Le même fichier, au même `content_hash`, sort de `tv4 ingest kraken`. Les exemples de sortie ci-dessous viennent de cette fixture.

### Schéma

```sql
DESCRIBE SELECT * FROM 'data/candles/kraken/BTC-USD/1m/*.parquet';
```

Sur la fixture : `open_time_ms BIGINT`, `open`, `high`, `low`, `close`, `volume` en `DOUBLE`, `trades UINTEGER`.

### Bougies par mois

```sql
SELECT strftime(epoch_ms(open_time_ms), '%Y-%m') AS mois,
       count(*) AS bougies,
       epoch_ms(min(open_time_ms)) AS premiere,
       epoch_ms(max(open_time_ms)) AS derniere
FROM 'data/candles/kraken/BTC-USD/1m/*.parquet'
GROUP BY mois
ORDER BY mois;
```

Sur la fixture : `2020-01 | 10000 | 2020-01-01 00:00:00 | 2020-01-08 21:29:00`.

### Trous les plus longs

```sql
SELECT epoch_ms(prev) AS avant,
       epoch_ms(open_time_ms) AS apres,
       (open_time_ms - prev) // 60000 - 1 AS minutes_absentes
FROM (SELECT open_time_ms, lag(open_time_ms) OVER (ORDER BY open_time_ms) AS prev
      FROM 'data/candles/kraken/BTC-USD/1m/*.parquet')
WHERE open_time_ms - prev > 60000
ORDER BY minutes_absentes DESC, apres
LIMIT 5;
```

Sur la fixture, première ligne : `2020-01-05 10:23:00 | 2020-01-05 10:32:00 | 8`. Les totaux de la même série de trous :

```sql
SELECT count(*) AS trous,
       max(open_time_ms - prev) - 60000 AS plus_long_ms,
       sum((open_time_ms - prev) // 60000 - 1) AS minutes_absentes
FROM (SELECT open_time_ms, lag(open_time_ms) OVER (ORDER BY open_time_ms) AS prev
      FROM 'data/candles/kraken/BTC-USD/1m/*.parquet')
WHERE open_time_ms - prev > 60000;
```

Sur la fixture : `951 | 480000 | 1370`, soit 951 trous, 480 000 ms pour le plus long et 1 370 minutes absentes, les valeurs du bloc `continuity` du manifeste.

### Rééchantillonnage en 1h

Seules les bougies clôturées sont gardées, avec la même règle que le moteur (une bougie 1h est clôturée quand sa fin est atteinte par la dernière minute disponible).

```sql
WITH m1 AS (SELECT * FROM 'data/candles/kraken/BTC-USD/1m/*.parquet'),
     fin AS (SELECT max(open_time_ms) + 60000 AS fin_ms FROM m1)
SELECT epoch_ms(open_time_ms - open_time_ms % 3600000) AS heure,
       arg_min(open, open_time_ms) AS open,
       max(high) AS high,
       min(low) AS low,
       arg_max(close, open_time_ms) AS close,
       sum(volume) AS volume,
       sum(trades) AS trades
FROM m1
GROUP BY open_time_ms - open_time_ms % 3600000
HAVING open_time_ms - open_time_ms % 3600000 + 3600000 <= (SELECT fin_ms FROM fin)
ORDER BY heure;
```

Sur la fixture : 189 heures (la bougie 1h de 21:00 le 08/01, incomplète, est exclue). Première ligne : `2020-01-01 00:00:00 | 7168.3 | 7170.5 | 7153.8 | 7155.7 | 70.76177528000001 | 267`. Elle est identique à la première bougie du 1h natif de Kraken (`fixtures/kraken_xbtusd_1h.csv`) à l'arrondi du volume près (`70.76177528` côté Kraken). `open_time_ms % 3600000` est correct parce que `open_time_ms` est positif (après 1970).

### Métadonnées Parquet

```sql
SELECT file_name, row_group_id, row_group_num_rows, path_in_schema, compression
FROM parquet_metadata('data/candles/kraken/BTC-USD/1m/*.parquet');
```

Sur la fixture : un groupe de lignes de 10 000 lignes et sept colonnes compressées en `ZSTD`. Le manifeste JSON voisin (`<période>._manifest.json`) porte le `content_hash`, la période, `closed` et le bloc `continuity` : ces informations ne sont pas dans les métadonnées Parquet, où seul `ARROW:schema` figure (`parquet_kv_metadata` n'apporte rien d'autre).

### Précautions

Le motif `*.parquet` lit aussi les fichiers de plusieurs mois ou années en une seule requête. Aucune des requêtes ci-dessus n'écrit dans `data/`. Il ne faut jamais écrire dans `data/` avec DuckDB : les fichiers clos sont immuables et vérifiés par hash.
