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

- `<run_dir>` contient les quatre fichiers écrits par `tv4 backtest` : `manifest.json` (`tv4-manifest-s8` ou `tv4-manifest-s9`), `metrics.json` (`tv4-metrics-s8` ou `tv4-metrics-s9`, de la même génération que le manifeste), `trades.csv` (36 colonnes, `exit_reason = quote_gap` et `exit_type = gap_forced_exit` compris) et `equity.csv`. Un run s9 (mode portefeuille) lit `data.symbols` à la place de `data.symbol` et ses blocs `funnel_by_pair`, `executability`, `g1_executable`, `regime`, `instruments`, `portfolio` et `priority`, tous facultatifs : le rapport ajoute les sections « Univers et entonnoir par paire » et « Exécutabilité (G0) et G1 », une colonne « Paire » aux trades et les lignes « Capital initial (hors `config_hash`) », « Univers », « Régime » et « Instruments » au lineage. Le rendu d'un bloc s9 absent ou nul porte « Ce run ne porte pas de bloc … » (sauf le régime, facultatif), que le job CI `rapport-bout-en-bout` recherche dans le rendu d'un vrai run. Les refus du plafond de positions (`funnel.entry_rejected_max_positions`, `signals.rejected_max_positions`, obligatoires en s9) ferment l'entonnoir global quand ils ne sont pas nuls. Un run s8 se rend à l'octet comme avant. Un fichier absent, un JSON invalide, un schéma inconnu, une clé manquante, un en-tête CSV inattendu ou un nombre mal formé arrêtent le rendu avec le nom du fichier, le chemin JSON ou la ligne CSV en cause.
- `--output` : défaut `<run_dir>/report.html`, écriture atomique.
- Codes de sortie : 0 rendu écrit ; 1 run invalide ou écriture impossible ; 2 erreur d'usage (argument manquant, option ou commande inconnue), posé par `App\Console\Tv4Application`, que Symfony sortirait sinon en 1.
- Environnement et debug, lus par `bin/console` comme avant Symfony Runtime (`App\Console\KernelFactory`) : `--env=<env>` ou `-e <env>` (défaut : variable `APP_ENV`, sinon `prod`) ; debug **actif par défaut** (`APP_DEBUG` absent), éteint par `APP_DEBUG=0` ou par `--no-debug`.
- Cache et fraîcheur : le cache Twig compilé est désactivé (`config/packages/twig.php`) et le debug actif par défaut fait vérifier à Symfony la fraîcheur des ressources du conteneur. Après un `git pull`, le rapport ne reflète donc jamais un template ou une configuration antérieurs. Avec `APP_DEBUG=0` ou `--no-debug`, le conteneur de `var/cache/<env>` n'est plus revérifié : vider `var/cache/` après chaque `git pull` (`rm -rf var/cache`).
- Le HTML est autonome (CSS en ligne, SVG calculés en PHP, aucun JavaScript, aucune ressource externe) et déterministe : aucune date de génération, aucun chemin absolu, deux rendus du même run sont identiques à l'octet.
- À `--equity-step 1m`, `equity.csv` approche le million de lignes : `php -d memory_limit=512M bin/console tv4:report …`.

Le control plane ne recalcule ni hash ni métrique : il affiche ce que `tv4` a écrit. Une section sans données est rendue comme absente, avec sa raison (portes G0 à G7, plis du walk-forward : jalons 1.5 et 2).

### Organisation

- `src/Run/` : lecture et validation (`RunDirectoryReader`, `Json/JsonObject`, `Csv/*`), DTO immuables dans `Run/Dto/`, `InvalidRunException`.
- `src/Report/` : mise en forme française (`Format`), graphiques SVG (`Chart/*`), `ReportRenderer`, `ReportWriter`.
- `src/Command/Tv4ReportCommand.php` : la commande ; `src/Console/` : `Tv4Application` (code de sortie 2 pour l'usage) et `KernelFactory` (environnement et debug).
- `templates/report/standalone.html.twig` (document complet, CSS) inclut `templates/run/fiche.html.twig` (fiche de run de `docs/05-ihm.md` §4, une section par partial dans `templates/run/sections/`), réutilisable telle quelle par la fiche du jalon 2a.
- `tests/fixtures/run-exemple/` : run d'exemple **fabriqué** (voir son `README.md`) : ce n'est pas une sortie de `tv4`.
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
