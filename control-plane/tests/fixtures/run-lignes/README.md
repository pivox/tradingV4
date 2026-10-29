# Dossier de lignes d'exemple (données fabriquées, schémas s11)

Ce dossier est un **dossier de lignes écrit à la main pour les tests** du control plane, selon `docs/specs/s11.md` §9.1 et §9.3. Ce n'est **ni une sortie de `tv4`, ni un résultat de backtest, ni des données de marché**. Il sera remplacé par la sortie réelle du golden Rust `tv4/tests/golden_runs/research_lines/` (lot E) avant la PR de S11.

- Il suit la forme d'un dossier écrit par `tv4 backtest` sur un dataset de bloc avec `--okx-subset` : `full/` et `okx_usdc/` (chacun un run s11 complet) puis `lines.json` (`tv4-lines-v1`, écrit à clés triées et à la fin).
- Même construction que `../run-recherche/` (dérivé du run s9, trade `end_of_window` purgé), pour le bloc **VALIDATION** (dix trades décalés au 2023-03-01) joué avec l'option cachée `--unguarded-protected-read` : `research.guard = "unguarded"` dans les deux runs, d'où le bandeau de garde.
- `full/` joue `BTC-USD` et `ETH-USD` ; `okx_usdc/` ne joue que `BTC-USD` (sous-ensemble `okx_usdc_tradable`, base `BTC`, `ETH-USD` retirée). Les deux runs ont le **même `config_hash`** et deux `run_id` (§9.1). Les trades de `okx_usdc/` sont ceux de `BTC-USD` de `full/`, et ses agrégats ont été recalculés par un script jetable (non versionné) ; le déroulé du portefeuille (cash partagé, `max_positions`) n'est pas rejoué, ce qui ne serait pas vrai d'un vrai run.
- `lines.json` : résumé de chaque ligne (arrondis à 10 décimales comme `profiles.json`), `residual_survivorship` = `full` − `okx_usdc`, valeurs recopiées des métriques par le script. Les hashes sont **factices** (motifs hexadécimaux répétés).

Autre fixture : `../run-lignes-profils/` (lignes qui sont des dossiers de profils de coûts).
