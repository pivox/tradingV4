# Run de recherche réel (sortie de `tv4`, schémas s11)

Ce dossier est la **sortie réelle de `tv4 backtest`** sur un dataset de bloc DEV, en symboles fixes, une seule ligne (`docs/specs/s11.md` §9.2). Les quatre fichiers (`manifest.json`, `metrics.json`, `trades.csv`, `equity.csv`) sont ceux que le binaire a écrits, sans retouche. Les données sont celles de `fixtures/kraken_xbtusd_1m.csv` (10 000 bougies 1m réelles de 2020-01) ; la stratégie est `sma_cross` avec les paramètres de démonstration de `config/strategies/sma_cross.toml`, qui ne sont pas une hypothèse de recherche. Le verdict est `rejected_too_few_trades` (4 trades) : ce n'est pas un résultat de stratégie.

Commandes (depuis la racine du dépôt, binaire debug ; la fumée s11 de `.github/workflows/ci.yml` joue la même chaîne) :

```zsh
tv4 ingest csv --root smoke-s11 --csv fixtures/kraken_xbtusd_1m.csv --exchange kraken --symbol BTC-USD --tf 1m
tv4 dataset build --root smoke-s11 --name fumee_dev --exchange kraken --symbol BTC-USD --tf 1m --block DEV
tv4 backtest --root smoke-s11 --dataset fumee_dev --strategy sma_cross --params config/strategies/sma_cross.toml --fees okx --symbols BTC-USD --sizing research --capital 300 --out run-recherche
```

- Bloc DEV, fenêtre de décision [2018-01-01, 2023-01-01), chauffe de 210 jours (`warmup_start` 2017-06-05), univers **fixe** (`research.universe.mode = "fixed"`, un seul symbole), `research.guard = "none"`, aucun trade purgé, `research_inputs` vide, règles d'instrument de `config/instruments.toml`.
- `engine` et `started_at` du manifeste sont ceux de la machine qui a produit la sortie (binaire debug, commit de la branche de S11). Le dataset de 7 jours étant entièrement dans le bloc, l'univers trimestriel, les exclusions qualité, la purge, la garde levée, la ligne `okx_usdc` et les profils de coûts n'y apparaissent pas : ils sont couverts par les fixtures écrites à la main `../run-recherche-trimestriel/`, `../run-lignes-garde/` et `../run-lignes-profils/` (voir `../README.md`).
