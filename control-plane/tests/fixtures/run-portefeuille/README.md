# Run portefeuille d'exemple (données fabriquées, schémas s9)

Ce dossier est un **run écrit à la main pour les tests** du control plane, selon `docs/specs/s9.md` §13 et §14.1. Ce n'est **ni une sortie de `tv4`, ni un résultat de backtest, ni des données de marché** : aucune valeur ne doit être lue comme un résultat de stratégie.

- Il suit les schémas `tv4-manifest-s9` et `tv4-metrics-s9`, le format de `trades.csv` en 36 colonnes (valeurs `quote_gap` et `gap_forced_exit` comprises) et le format de `equity.csv` de s8.
- Il dérive du run d'exemple s8 (`../run-exemple/`) : mêmes dix trades, répartis entre `BTC-USD` (lots 1, 3, 5, 6, 7 et 10) et `ETH-USD` (lots 2, 4, 8 et 9), dont le lot 9 est une sortie forcée sur trou de cotation. Les prix d'`ETH-USD` sont donc ceux de la fixture s8 et n'ont aucun sens de marché. Les statistiques par paire et par type de sortie ont été recalculées à partir des lignes de `trades.csv` par un script jetable (non versionné).
- Le bloc `data` suit §13.1 (`symbols`, `regime_symbol`, `series[]`, sans `bars`, `synthetic_bars` ni `minutes_missing`) ; `window` et `data.bars` n'existent plus dans `metrics.json`, ce qui exerce les valeurs nulles du lecteur. `regime` vaut `null` : la stratégie `sma_cross` ne lit pas le régime (les tests injectent un bloc de régime par remplacement de texte).
- Les **hashes sont factices** (motifs hexadécimaux répétés, de forme valide) et `manifest.outputs.files` ne correspond à aucun contenu réel : le control plane ne recalcule aucun hash (rôle de `tv4 verify`). Le fichier d'instruments cité (`instruments/okx/spot-20261028T124500Z.json`) n'existe pas.
- L'exécutabilité (G0) et G1 sont inventées : `BTC-USD` passe, `ETH-USD` est écartée pour volume, et G1 est à 10/11 des entrées exécutables (`fail`).
- Il a **exactement la forme réelle** d'un run s9 de `tv4` : mêmes clés, mêmes types (`data.source = "dataset"` et `data.dataset`, `config.schema`, `config.instruments_hash`, `sim_by_pair`, `funnel.entry_rejected_max_positions`, `signals.rejected_max_positions`, ces deux derniers non nuls avec les identités de l'entonnoir gardées). Seule exception voulue : `instruments.upcoming_changes` n'est pas vide ici, alors qu'il l'est dans les runs de `tv4` actuels.

## Pourquoi une fixture écrite à la main

Elle seule couvre `gap_forced_exit`, `upcoming_changes`, `not_live` (ETH-USDC écartée) et des refus `max_positions` répartis entre deux paires. Le golden `tv4/tests/golden_runs/portfolio_2legs/` n'a pas de manifeste (le test Rust `le_golden_est_complet_et_sans_manifeste` l'exige) : il ne peut donc pas remplacer ce dossier tel quel.

## Comment sa conformité aux clés réelles est contrôlée

1. **Test PHPUnit sur la vraie sortie** : `tests/Run/GoldenPortfolioRunTest.php` lit `metrics.json`, `trades.csv` et `equity.csv` du golden `portfolio_2legs`, y ajoute un `manifest.json` écrit par le test (forme du manifeste réel, hashes alignés sur ceux du golden) et vérifie que les blocs `funnel_by_pair`, `portfolio`, `priority`, `instruments`, `executability`, `g1_executable` et `distribution.by_pair` sont lus (non nuls), que `regime` est nul (`sma_cross` ne lit pas le régime), et que le rendu ne contient aucun texte d'absence. Aucune valeur numérique du golden n'y est épinglée : il peut être régénéré, un bloc renommé côté Rust ferait échouer le test.
2. **Job CI `rapport-bout-en-bout`** : l'étape « Contrôler les blocs s9 du rapport du portefeuille » échoue si le rendu du run de fumée contient « Ce run ne porte pas de bloc <code>…</code> » pour l'un des blocs obligatoires (le régime, facultatif, est exclu).
3. **Comparaison des clés** : après un changement de schéma côté `tv4`, comparer les chemins de clés de cette fixture à ceux d'un run réel (golden ou run de fumée) et compléter la fixture.
