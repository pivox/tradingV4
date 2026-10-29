# control-plane

Control plane Symfony de tradingV4. Le moteur (`tv4/`, Rust) calcule ; ce répertoire présente et pilote. Il ne contient aucune logique de backtest et ne recalcule aucun résultat.

**État : squelette du jalon 1 (étape S8, lot C).** Une seule commande, `tv4:report`, qui rend le rapport HTML statique d'un run produit par `tv4`. Rien d'autre n'est codé ici.

## Jalon 1 : squelette minimal de rendu du rapport

- Projet Symfony minimal, sans base de données, sans serveur web exposé.
- Une commande console `tv4:report <run_dir>` lit les fichiers produits par `tv4` dans `runs/<run_id>/` : `manifest.json`, `metrics.json` et `trades.csv` (ainsi que `equity.csv` et les sorties de `research::stats` quand elles existent).
- Elle rend un rapport HTML statique avec des templates Twig, organisés selon les sections de la fiche de run de `docs/05-ihm.md` pour être réutilisés tels quels par la fiche de run du jalon 2a.
- Les hashes (`config_hash`, `dataset_hash`, `result_hash`) et le commit du moteur sont affichés tels que `tv4` les a écrits : `tv4` reste la source de vérité (`docs/03-architecture.md` §15.2). Le format `dsh-v1` est spécifié dans `docs/dataset-hash.md` pour permettre une vérification indépendante.

Le rapport jetable côté moteur (templating Rust) est écarté (`docs/05-ihm.md` §7, point 3).

### Installation et usage

Prérequis : PHP 8.3 et Composer 2. Symfony **7.4 LTS** (la 7.3 n'a plus de version publiée, la 8.x exige PHP 8.4.1). Le squelette est écrit à la main : ni `symfony/flex`, ni `symfony/runtime`, aucun fichier `.env`, aucune base de données.

```sh
cd control-plane
composer install --no-interaction
php bin/console tv4:report <run_dir> [--output <fichier.html>] [--env <env>] [--no-debug]
```

- `<run_dir>` contient les quatre fichiers écrits par `tv4 backtest` : `manifest.json` (`tv4-manifest-s8`, `tv4-manifest-s9`, `tv4-manifest-s10` ou `tv4-manifest-s11`), `metrics.json` (`tv4-metrics-s8`, `tv4-metrics-s9`, `tv4-metrics-s10` ou `tv4-metrics-s11`, de la même génération que le manifeste), `trades.csv` (36 colonnes, `exit_reason = quote_gap` et `exit_type = gap_forced_exit` compris) et `equity.csv`. Un run s9 (mode portefeuille) lit `data.symbols` à la place de `data.symbol` et ses blocs `funnel_by_pair`, `executability`, `g1_executable`, `regime`, `instruments`, `portfolio` et `priority`, tous facultatifs : le rapport ajoute les sections « Univers et entonnoir par paire » et « Exécutabilité (G0) et G1 », une colonne « Paire » aux trades et les lignes « Capital initial (hors `config_hash`) », « Univers », « Régime » et « Instruments » au lineage. Le rendu d'un bloc s9 absent ou nul porte « Ce run ne porte pas de bloc … » (sauf le régime, facultatif), que le job CI `rapport-bout-en-bout` recherche dans le rendu d'un vrai run. Les refus du plafond de positions (`funnel.entry_rejected_max_positions`, `signals.rejected_max_positions`, obligatoires en s9) ferment l'entonnoir global quand ils ne sont pas nuls. Un run s10 (portefeuille joué avec un profil de coûts, `--costs`) se lit comme un run s9, plus les blocs obligatoires `costs`, `cost_arithmetic` et `cost_counters` (leur absence arrête le rendu) ; `fees` et `slippage_bps` n'y existent plus, la grille de frais se lit dans `costs.fee_grid` de la config, et `trades.csv` porte `spread_cost`. Sa fiche ajoute une section « Coûts » (paramètres résolus par jambe et leurs sources, arithmétique de `docs/04-protocole-recherche.md` §3.3, compteurs), la mention « Stress non calibré » tant que `costs.stress_calibrated` vaut `false`, des lignes de lineage (profil, `costs_file_hash`) et une colonne « Spread » aux trades. Un run s8 se rend à l'octet comme avant, et un run s9 aussi (`tests/fixtures/rapport-s8-attendu.html`, `tests/fixtures/rapport-s9-attendu.html`).
- Un dossier qui contient `profiles.json` (`tv4-profiles-v1`) est un **dossier de profils** : `tv4:report <dossier>` lit `profiles.json` et les sous-dossiers `baseline/`, `adverse/` et `maker/` (chacun un run s10, recoupé avec `profiles.json` : identifiants, hashes, profil, graine, grille) et rend la section « Profils de coûts » (étiquette, avec « dépend du fill » en évidence, tableau des trois profils, portes croisées sur baseline et adverse, statut du profil maker et part de ses entrées remplies en maker), puis la fiche complète du profil baseline. Les fiches adverse et maker se rendent depuis leurs sous-dossiers. Le control plane n'en recalcule ni l'étiquette ni les portes. Un fichier absent, un JSON invalide, un schéma inconnu, une clé manquante, un en-tête CSV inattendu ou un nombre mal formé arrêtent le rendu avec le nom du fichier, le chemin JSON ou la ligne CSV en cause.
- Un run s11 (portefeuille joué sur un dataset de recherche par bloc, `docs/specs/s11.md` §9.2) se lit comme un run s9, plus le bloc **obligatoire** `research` de `metrics.json` (bloc DEV, VALIDATION, HOLDOUT ou FORWARD, fenêtre de décision, univers trimestriel ou fixe, ligne `full` ou `okx_usdc`, sous-ensemble, identité de recherche et son hash, exclusions qualité, purge, refus d'entrée de la recherche, garde), `data.block`, `data.decision_window` et `research_inputs` du manifeste, et `config.research`. Les blocs de coûts de s10 (`costs`, `cost_arithmetic`, `cost_counters`) sont lus si et seulement si `metrics.json` porte `costs` (run joué avec `--costs`). Le lecteur recoupe le bloc `research` avec le reste du run : l'identité répète le bloc, la ligne et la fenêtre, `research.purge.trades_kept` égale `n_trades` (les métriques de trades portent sur les trades conservés), `trades.csv` garde tous les trades et ceux en `end_of_window` sont les purgés. La fiche ajoute une section « Recherche » avant le verdict (fenêtre, univers, identité de recherche, purge, refus d'entrée par jambe, trimestres, exclusions qualité, bandeau si `research.guard = "unguarded"`) et deux lignes de lineage ; elle est absente des rendus s8, s9 et s10, qui restent identiques à l'octet (`tests/fixtures/rapport-s8-attendu.html`, `rapport-s9-attendu.html`, `rapport-s10-attendu.html`).
- Un dossier qui contient `lines.json` (`tv4-lines-v1`, `docs/specs/s11.md` §9.3) est un **dossier de lignes** : `tv4:report <dossier>` lit `lines.json` et les sous-dossiers `full/` et `okx_usdc/` (chacun un run s11, ou un dossier de profils de coûts dont les trois runs sont s11), recoupe `lines.json` avec eux (identifiants, bloc, ligne, hashes, capital, jambes, sous-ensemble, effectif, purge, verdict ; les deux lignes ont le même `config_hash` par profil), puis rend la section « Deux lignes » (tableau univers complet, sous-ensemble OKX USDC et survivance résiduelle par profil) suivie de la fiche de la ligne `full` (profil baseline, précédé de la section « Profils de coûts », quand les lignes sont des dossiers de profils). La fiche de la ligne `okx_usdc` se rend depuis son sous-dossier. L'ordre de détection est `lines.json`, puis `profiles.json`, puis run simple. Le control plane ne recalcule rien, la survivance résiduelle comprise.
- `--output` : défaut `<run_dir>/report.html`, écriture atomique.
- Codes de sortie : 0 rendu écrit ; 1 run invalide ou écriture impossible ; 2 erreur d'usage (argument manquant, option ou commande inconnue), posé par `App\Console\Tv4Application`, que Symfony sortirait sinon en 1.
- Environnement et debug, lus par `bin/console` comme avant Symfony Runtime (`App\Console\KernelFactory`) : `--env=<env>` ou `-e <env>` (défaut : variable `APP_ENV`, sinon `prod`) ; debug **actif par défaut** (`APP_DEBUG` absent), éteint par `APP_DEBUG=0` ou par `--no-debug`.
- Cache et fraîcheur : le cache Twig compilé est désactivé (`config/packages/twig.php`) et le debug actif par défaut fait vérifier à Symfony la fraîcheur des ressources du conteneur. Après un `git pull`, le rapport ne reflète donc jamais un template ou une configuration antérieurs. Avec `APP_DEBUG=0` ou `--no-debug`, le conteneur de `var/cache/<env>` n'est plus revérifié : vider `var/cache/` après chaque `git pull` (`rm -rf var/cache`).
- Le HTML est autonome (CSS en ligne, SVG calculés en PHP, aucun JavaScript, aucune ressource externe) et déterministe : aucune date de génération, aucun chemin absolu, deux rendus du même run sont identiques à l'octet.
- À `--equity-step 1m`, `equity.csv` approche le million de lignes : `php -d memory_limit=512M bin/console tv4:report …`.

Le control plane ne recalcule ni hash ni métrique : il affiche ce que `tv4` a écrit. Une section sans données est rendue comme absente, avec sa raison (portes G0 à G7, plis du walk-forward : jalons 1.5 et 2).

### Organisation

- `src/Run/` : lecture et validation (`RunDirectoryReader`, `ProfilesDirectoryReader`, `LinesDirectoryReader`, `Json/JsonObject`, `Csv/*`), DTO immuables dans `Run/Dto/`, `InvalidRunException`.
- `src/Report/` : mise en forme française (`Format`), graphiques SVG (`Chart/*`), `ReportRenderer`, `ReportWriter`.
- `src/Command/Tv4ReportCommand.php` : la commande ; `src/Console/` : `Tv4Application` (code de sortie 2 pour l'usage) et `KernelFactory` (environnement et debug).
- `templates/report/standalone.html.twig` (document complet, CSS) inclut `templates/run/fiche.html.twig` (fiche de run de `docs/05-ihm.md` §4, une section par partial dans `templates/run/sections/`), réutilisable telle quelle par la fiche du jalon 2a.
- `tests/fixtures/run-exemple/` : run d'exemple **fabriqué** (voir son `README.md`) : ce n'est pas une sortie de `tv4`.
- `tests/fixtures/run-profils/` : dossier de profils s10 **écrit à la main** (voir son `README.md`), de la forme d'un dossier de `tv4 backtest --costs` ; `tests/Run/GoldenProfilesRunTest.php` lit le golden `tv4/tests/golden_runs/portfolio_profiles/` de s10 (sans manifeste : le test en écrit un par profil dans un dossier temporaire), et sa conformité aux clés réelles est aussi contrôlée par le job CI `rapport-bout-en-bout`.
- `tests/fixtures/run-recherche/` et `run-lignes/` : **sorties réelles** de `tv4 backtest` (dataset de bloc DEV construit sur `fixtures/kraken_xbtusd_1m.csv`, symboles fixes, avec et sans `--okx-subset`), copiées sans retouche avec leurs manifestes (voir leurs `README.md` pour les commandes). `run-recherche-trimestriel/`, `run-lignes-garde/` et `run-lignes-profils/` : run s11 et dossiers de lignes **écrits à la main**, gardés pour ce que cette sortie ne montre pas (univers trimestriel, exclusions qualité, trade purgé, garde levée, jambes qui diffèrent d'une ligne à l'autre, lignes de profils de coûts) ; leurs clés sont alignées sur ce que `tv4` écrit, contrôle fait par `tests/Run/HandMadeFixturesKeysTest.php`, et seules les valeurs sont fabriquées. `tests/Run/GoldenResearchLinesTest.php` lit le golden `tv4/tests/golden_runs/research_lines/` (il écrit un manifeste s11 par run, le golden n'en ayant pas) et ne saute jamais. Récapitulatif de toutes les fixtures : `tests/fixtures/README.md`.
- `tests/fixtures/run-portefeuille/` : run s9 **écrit à la main**, de la forme exacte d'un run de `tv4` (voir son `README.md`) ; sa conformité aux clés réelles est contrôlée par `tests/Run/GoldenPortfolioRunTest.php` (golden `portfolio_2legs`) et par le job CI `rapport-bout-en-bout`.

### Qualité

```sh
composer validate --strict
composer test     # PHPUnit 12
composer stan     # PHPStan niveau max sur src/ et tests/
composer cs       # PHP_CodeSniffer, standard PSR-12
```

`composer.lock` est commité ; `vendor/`, `var/` et `config/reference.php` (généré par Symfony) sont ignorés. Le job CI `control-plane` lance ces commandes ; le job `rapport-bout-en-bout` rend le rapport d'un vrai run de fumée de `tv4` pour contrôler la conformité des DTO.

## Jalon 2 : control plane

- API Platform en lecture et Doctrine sur PostgreSQL.
- Les tables `datasets` et `runs` sont remplies par import des manifestes JSON et des CSV des runs (`docs/03-architecture.md` §5.4).
- Écran de lancement des runs, fiches de recherche et compteurs d'essais (`docs/04-protocole-recherche.md`, `docs/05-ihm.md`).

## Contraintes permanentes

- Un seul utilisateur, accès par VPN seul, login et TOTP à l'intérieur du VPN, aucune notification sortante (`docs/05-ihm.md` §7).
- Jamais de secret dans le dépôt : aucun fichier `.env` (y compris celui que Symfony génère par défaut), aucune clé API.
