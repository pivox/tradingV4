# Fixtures du control plane

Ce dossier réunit les runs que les tests du control plane lisent et rendent. Un run **réel** est la sortie de `tv4`, copiée sans retouche ; un run **écrit à la main** est fabriqué pour les tests (valeurs inventées, hashes factices), selon la spécification de son étape. Aucune valeur de ces dossiers n'est un résultat de stratégie ni une donnée de marché fabriquée pour être crue.

| Dossier | Origine | Génération | Ce qu'il montre |
|---|---|---|---|
| `run-exemple/` | écrit à la main | s8 | un run mono-paire |
| `run-portefeuille/` | écrit à la main, de la forme exacte d'un run de `tv4` | s9 | un run portefeuille à deux jambes |
| `run-profils/` | écrit à la main | s10 | un dossier de profils de coûts |
| `run-recherche/` | **réel** : `tv4 backtest` sur un dataset de bloc DEV tiré de `fixtures/kraken_xbtusd_1m.csv`, symboles fixes | s11 | un run de recherche à univers fixe, une ligne, sans trade purgé |
| `run-lignes/` | **réel** : même dataset, `--okx-subset` | s11 | un dossier de lignes (`full/`, `okx_usdc/`, `lines.json`) |
| `run-recherche-trimestriel/` | écrit à la main | s11 | univers trimestriel, exclusions qualité, trade purgé |
| `run-lignes-garde/` | écrit à la main | s11 | garde levée (`--unguarded-protected-read`), jambes qui diffèrent d'une ligne à l'autre |
| `run-lignes-profils/` | écrit à la main | s11 | lignes qui sont des dossiers de profils de coûts |
| `rapport-s8-attendu.html`, `rapport-s9-attendu.html`, `rapport-s10-attendu.html` | rendu figé de `run-exemple/`, `run-portefeuille/` et `run-profils/` | | les rendus s8, s9 et s10 sont identiques à l'octet (`tests/Command/Tv4ReportResearchCommandTest.php`) |

Les goldens Rust (`tv4/tests/golden_runs/`) sont lus directement par `tests/Run/Golden*Test.php` : ce sont des sorties de bibliothèque, sans manifeste, que le test complète.

Conformité des fixtures écrites à la main : `tests/Run/HandMadeFixturesKeysTest.php` refuse toute clé de leurs JSON de la génération s11 qui n'existe pas dans une sortie réelle versionnée (fixtures réelles ci-dessus et goldens Rust). Pour les produire à nouveau, rejouer les commandes du `README.md` de chaque fixture réelle avec le binaire debug ; les manifestes portent alors le commit et l'heure de la machine.
