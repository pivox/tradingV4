# Dossier de lignes réel (sortie de `tv4`, schémas s11)

Ce dossier est la **sortie réelle de `tv4 backtest --okx-subset`** sur le dataset de bloc DEV de `../run-recherche/` : `full/`, `okx_usdc/` (chacun un run s11 complet, avec son manifeste) puis `lines.json` (`tv4-lines-v1`, `docs/specs/s11.md` §9.3). Aucun fichier n'est retouché. Mêmes données et même stratégie que `../run-recherche/` (fixture Kraken réelle, `sma_cross` de démonstration, verdict `rejected_too_few_trades`) : aucune valeur n'est un résultat de stratégie.

Le sous-ensemble est celui de `tv4 universe okx-subset` sur la fixture d'instruments de test `tv4/tests/fixtures/okx_instruments_spot.json` (valeurs inventées : bases BTC, ETH et SOL). Commandes, depuis la racine du dépôt (le fichier du sous-ensemble est écrit dans un dossier existant, voir `docs/specs/s11.md` §17) :

```zsh
mkdir -p smoke-s11/subset
tv4 universe okx-subset --instruments tv4/tests/fixtures/okx_instruments_spot.json --out smoke-s11/subset/okx_usdc.json
tv4 backtest --root smoke-s11 --dataset fumee_dev --strategy sma_cross --params config/strategies/sma_cross.toml --fees okx --symbols BTC-USD --okx-subset smoke-s11/subset/okx_usdc.json --sizing research --capital 300 --out run-lignes
```

Le manifeste de `okx_usdc/` garde le chemin tel que donné à la commande (`research_inputs.okx_subset` = `smoke-s11/subset/okx_usdc.json`). Une seule jambe, BTC-USD, est tradable : les deux lignes jouent les mêmes trades, ont deux `run_id`, le même `config_hash`, et la survivance résiduelle est nulle. Les cas où les lignes diffèrent sont couverts par la fixture écrite à la main `../run-lignes-garde/`.
