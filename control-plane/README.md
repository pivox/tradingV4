# control-plane

Control plane Symfony de tradingV4. Le moteur (`tv4/`, Rust) calcule ; ce répertoire présente et pilote. Il ne contient aucune logique de backtest et ne recalcule aucun résultat.

**État : rien n'est encore codé ici.** Le squelette arrive à l'étape S8 du jalon 1 (`docs/03-architecture.md` §11, point 8).

## Jalon 1 : squelette minimal de rendu du rapport

- Projet Symfony minimal, sans base de données, sans serveur web exposé.
- Une commande console `tv4:report <run_dir>` lit les fichiers produits par `tv4` dans `runs/<run_id>/` : `manifest.json`, `metrics.json` et `trades.csv` (ainsi que `equity.csv` et les sorties de `research::stats` quand elles existent).
- Elle rend un rapport HTML statique avec des templates Twig, organisés selon les sections de la fiche de run de `docs/05-ihm.md` pour être réutilisés tels quels par la fiche de run du jalon 2a.
- Les hashes (`config_hash`, `dataset_hash`, `result_hash`) et le commit du moteur sont affichés tels que `tv4` les a écrits : `tv4` reste la source de vérité (`docs/03-architecture.md` §15.2). Le format `dsh-v1` est spécifié dans `docs/dataset-hash.md` pour permettre une vérification indépendante.

Le rapport jetable côté moteur (templating Rust) est écarté (`docs/05-ihm.md` §7, point 3).

## Jalon 2 : control plane

- API Platform en lecture et Doctrine sur PostgreSQL.
- Les tables `datasets` et `runs` sont remplies par import des manifestes JSON et des CSV des runs (`docs/03-architecture.md` §5.4).
- Écran de lancement des runs, fiches de recherche et compteurs d'essais (`docs/04-protocole-recherche.md`, `docs/05-ihm.md`).

## Contraintes permanentes

- Un seul utilisateur, accès par VPN seul, login et TOTP à l'intérieur du VPN, aucune notification sortante (`docs/05-ihm.md` §7).
- Jamais de secret dans le dépôt : aucun fichier `.env` (y compris celui que Symfony génère par défaut), aucune clé API.
