# tradingV4

Plateforme de trading crypto spot sur OKX : un moteur Rust de backtest, de paper trading et d'exécution, et un control plane Symfony. La conception complète, l'architecture et le protocole de recherche sont dans `docs/` (point d'entrée : `docs/01-cadrage.md`).

Les décisions du propriétaire et les règles de travail du dépôt sont dans `CLAUDE.md`. Le moteur se construit et se teste depuis la racine avec `cargo build --release` et `cargo test --locked --all-targets`.

## Backtest, preuve et rapport (S8)

Depuis la racine du dépôt, sur la fixture de 10 000 bougies (le dataset `btcusd_2y` de la preuve se construit sur le poste du propriétaire, `docs/03-architecture.md` §11 S8) :

```sh
# Run : metrics.json, trades.csv, equity.csv puis manifest.json (écrit en dernier) dans --out
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

# Coût de la boucle par 1m et coût moyen par appel de on_bar (mêmes entrées que backtest, rien d'écrit)
cargo run --release --bin tv4 -- bench --fixture fixtures/kraken_xbtusd_1m.csv --strategy sma_cross \
  --params config/strategies/sma_cross.toml --repeat 5

# Rapport HTML statique d'un run (control plane Symfony, PHP 8.3)
(cd control-plane && composer install && php bin/console tv4:report ../runs/exemple-a --output ../runs/exemple-a/report.html)
```

Le rapport s'écrit par défaut dans `<run_dir>/report.html`. Codes de sortie de `tv4:report` : 0 rendu écrit, 1 run invalide ou écriture impossible, 2 erreur d'usage (argument manquant, option ou commande inconnue). L'environnement se règle par `--env`/`-e` (sinon `APP_ENV`, sinon `prod`) ; le debug est actif par défaut, `APP_DEBUG=0` ou `--no-debug` le coupe, et dans ce cas il faut vider `var/cache/` du control plane après un `git pull` (le cache Twig compilé est désactivé, mais pas le conteneur Symfony). Détail : `control-plane/README.md`.

`tv4 verify` ne lit du manifeste que les hashes annoncés (cohérence interne), `config_hash`, `dataset_hash` et `engine` à titre d'information ; `timings`, `started_at` et `run_id` sont ignorés.

Contrôles de reproductibilité en CI (`.github/workflows/ci.yml`) : le backtest de fumée tourne deux fois en deux process puis `tv4 verify` ; les deux goldens de run `tv4/tests/golden_runs/sma_cross_15m_okx/` (fumée, 2 trades) et `tv4/tests/golden_runs/sma_cross_1m_court_okx/` (49 trades, verdict, distribution) détectent la dérive entre commits, le bloc `approx` à `approx::ULP_TOLERANCE` ULP (20) et le reste à l'octet (mise à jour des deux : `TV4_GOLDEN_UPDATE=1 cargo test --test golden_run`, refusée si `CI` est définie, puis relecture du diff) ; le bench criterion `engine_2y_1m` (`cargo bench --locked --bench engine -- engine_2y_1m`) est archivé et sa borne basse est limitée à 50 ns par 1m par `.github/scripts/criterion-gate.sh`. La spécification de S8 est `docs/specs/s8.md` ; les points qu'elle laisse à la validation du propriétaire sont en §17.

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
