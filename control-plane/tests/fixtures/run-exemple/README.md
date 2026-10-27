# Run d'exemple (données fabriquées)

Ce dossier est un **run écrit pour les tests** du control plane. Ce n'est **ni une sortie de `tv4`, ni un résultat de backtest, ni des données de marché** : aucune valeur ne doit être lue comme un résultat de stratégie.

- Les quatre fichiers (`manifest.json`, `metrics.json`, `trades.csv`, `equity.csv`) suivent les schémas `tv4-manifest-s8` et `tv4-metrics-s8` et le format de `trades.csv` en 36 colonnes (`docs/03-architecture.md` §5.2 et §7.3, `docs/specs/s8.md` §2 à §5).
- Les prix suivent une sinusoïde autour de 29 000 ; les 10 trades, leurs frais, leur equity et les métriques qui en découlent ont été calculés par un script jetable (non versionné) à partir de ce tracé, pour que les chiffres restent cohérents entre eux (somme des PnL, drawdowns, histogramme de R, entonnoir).
- Les **hashes sont factices** : des motifs hexadécimaux répétés, de forme valide (`config_hash` et `run_id` en 64 hex nus, `dataset_hash`, `result_hash`, `binary_hash` et `outputs.files` en `blake3:<64 hex>`), qui ne correspondent à aucun contenu. `git_sha` et `binary_hash` sont inventés. Le dataset s'appelle `exemple_fabrique`.
- `manifest.outputs.files` ne correspond donc pas aux octets réels des fichiers : le control plane ne recalcule aucun hash (c'est le rôle de `tv4 verify`).
- Le verdict `rejected_too_few_trades` découle de 10 trades pour `min_trades = 30`.

La conformité avec une vraie sortie de `tv4` est contrôlée en CI par le job `rapport-bout-en-bout`, qui rend le rapport d'un run de fumée produit par le moteur.
