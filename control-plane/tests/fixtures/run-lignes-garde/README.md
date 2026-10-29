# Dossier de lignes à garde levée (écrit à la main, données fabriquées, schémas s11)

Ce dossier est un **dossier de lignes écrit à la main pour les tests** du control plane, selon `docs/specs/s11.md` §9.1 et §9.3. Ce n'est **ni une sortie de `tv4`, ni un résultat de backtest, ni des données de marché**. La sortie réelle d'un dossier de lignes sur la fixture Kraken est `../run-lignes/` ; celui-ci est gardé parce que cette sortie ne montre ni garde levée, ni jambes qui diffèrent d'une ligne à l'autre, ni trade purgé.

- Il suit la forme d'un dossier écrit par `tv4 backtest` sur un dataset de bloc avec `--okx-subset` : `full/` et `okx_usdc/` (chacun un run s11 complet) puis `lines.json` (`tv4-lines-v1`, écrit à clés triées et à la fin).
- Même construction que `../run-recherche-trimestriel/` (dérivé du run s9, trade `end_of_window` purgé), pour le bloc **VALIDATION** (dix trades décalés au 2023-03-01) joué avec l'option cachée `--unguarded-protected-read` : `research.guard = "unguarded"` dans les deux runs, d'où le bandeau de garde.
- `full/` joue `BTC-USD` et `ETH-USD` ; `okx_usdc/` ne joue que `BTC-USD` (sous-ensemble `okx_usdc_tradable`, base `BTC`, `ETH-USD` retirée). Les deux runs ont le **même `config_hash`** et deux `run_id` (§9.1). Les trades de `okx_usdc/` sont ceux de `BTC-USD` de `full/`, et ses agrégats ont été recalculés par un script jetable (non versionné) ; le déroulé du portefeuille (cash partagé, `max_positions`) n'est pas rejoué, ce qui ne serait pas vrai d'un vrai run.
- `lines.json` : résumé de chaque ligne (arrondis à 10 décimales comme `profiles.json`), `residual_survivorship` = `full` − `okx_usdc`, valeurs recopiées des métriques par le script. Les hashes sont **factices** (motifs hexadécimaux répétés).

Les clés sont alignées sur la sortie réelle (univers trimestriel, règles d'instrument d'`instruments_kraken.toml`, G0 et G1 `not_applicable`) : `tests/Run/HandMadeFixturesKeysTest.php` le contrôle. Seules les valeurs sont fabriquées.

Autre fixture : `../run-lignes-profils/` (lignes qui sont des dossiers de profils de coûts).
