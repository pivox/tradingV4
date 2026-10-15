# tradingV4

Plateforme de trading crypto spot sur OKX : un moteur Rust de backtest, de paper trading et d'exécution, et un control plane Symfony. La conception complète, l'architecture et le protocole de recherche sont dans `docs/` (point d'entrée : `docs/01-cadrage.md`).

Les décisions du propriétaire et les règles de travail du dépôt sont dans `CLAUDE.md`. Le moteur se construit et se teste depuis la racine avec `cargo build --release` et `cargo test --locked --all-targets`.

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
