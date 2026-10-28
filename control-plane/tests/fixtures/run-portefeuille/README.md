# Run portefeuille d'exemple (données fabriquées, schémas s9)

Ce dossier est un **run écrit à la main pour les tests** du control plane, selon `docs/specs/s9.md` §13 et §14.1. Ce n'est **ni une sortie de `tv4`, ni un résultat de backtest, ni des données de marché** : aucune valeur ne doit être lue comme un résultat de stratégie.

- Il suit les schémas `tv4-manifest-s9` et `tv4-metrics-s9`, le format de `trades.csv` en 36 colonnes (valeurs `quote_gap` et `gap_forced_exit` comprises) et le format de `equity.csv` de s8.
- Il dérive du run d'exemple s8 (`../run-exemple/`) : mêmes dix trades, répartis entre `BTC-USD` (lots 1, 3, 5, 6, 7 et 10) et `ETH-USD` (lots 2, 4, 8 et 9), dont le lot 9 est une sortie forcée sur trou de cotation. Les prix d'`ETH-USD` sont donc ceux de la fixture s8 et n'ont aucun sens de marché. Les statistiques par paire et par type de sortie ont été recalculées à partir des lignes de `trades.csv` par un script jetable (non versionné).
- Le bloc `data` suit §13.1 (`symbols`, `regime_symbol`, `series[]`, sans `bars`, `synthetic_bars` ni `minutes_missing`) ; `window` et `data.bars` n'existent plus dans `metrics.json`, ce qui exerce les valeurs nulles du lecteur. `regime` vaut `null` : la stratégie `sma_cross` ne lit pas le régime (les tests injectent un bloc de régime par remplacement de texte).
- Les **hashes sont factices** (motifs hexadécimaux répétés, de forme valide) et `manifest.outputs.files` ne correspond à aucun contenu réel : le control plane ne recalcule aucun hash (rôle de `tv4 verify`). Le fichier d'instruments cité (`instruments/okx/spot-20261028T124500Z.json`) n'existe pas.
- L'exécutabilité (G0) et G1 sont inventées : `BTC-USD` passe, `ETH-USD` est écartée pour volume, et G1 est à 10/11 des entrées exécutables (`fail`).

Ce dossier sera remplacé par la copie du golden `tv4/tests/golden_runs/portfolio_2legs/` à la fusion du lot F (spec §14.3).
