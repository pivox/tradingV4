# tradingV4 — Proposition d'IHM

Date : 2026-10-13, révision 2 après critique adversariale (trois lectures : opérateur sous stress, chercheur quant, UX et faisabilité ; 36 constats, 16 amendements intégrés ici, 20 reportés au jalon concerné en §8). Maquette cliquable : « tradingV4 Cockpit », hors dépôt.

## 1. À qui sert l'IHM, et à quoi

Un seul utilisateur, Haythem (décision du 2026-10-13 : pas de second utilisateur). Il alterne entre deux rôles. Conséquence : pas de gestion de rôles ni de journal nominatif ; le journal garde seulement l'origine de chaque commande (web, fichier kill sur la machine, chien de garde, exchange : les quatre étages du kill switch, 08 §C.4).

| Rôle | Ce qu'il fait dans l'IHM | Fréquence |
|---|---|---|
| **Chercheur** | Pré-enregistre une hypothèse, lance un backtest ou un walk-forward, lit le verdict contre les portes, compare des runs, promeut une stratégie en paper | Sessions de travail, plusieurs fois par semaine |
| **Opérateur** | Vérifie en 10 secondes que tout va bien (flux, bots, PnL, risque), lit pourquoi un bot n'a pas tradé, coupe les entrées ou sort du marché si nécessaire | Coups d'œil quotidiens, parfois à 3 h du matin depuis le téléphone |

Ce que l'IHM n'est pas : un terminal de trading manuel. On ne saisit jamais un prix ni une quantité (V3 avait un formulaire d'ordre manuel, c'est une voie d'exécution hors gates). Les seules commandes de marché sont des commandes de risque : couper les entrées, fermer une position ou tout fermer au marché, par le chemin d'exécution du bot, journalisées.

## 2. Principes

1. **Tout chiffre affiché est net de frais**, avec la même définition du PnL que le moteur ([04-protocole-recherche.md](04-protocole-recherche.md)). Devise de référence choisie en Réglages (€) ; les paires cotées en USDC sont converties par la paire USDC-EUR, au taux affiché avec son horodatage, et les frais restent visibles dans leur devise d'origine.
2. **Chaque décision est expliquée.** Un bot qui n'a pas tradé affiche la règle qui l'a bloqué (réponse au diagnostic « 0 trade » de V3 : 190 cas sur 191 inexpliqués).
3. **Les marches sont visibles partout.** Une stratégie monte les marches du protocole ([04-protocole-recherche.md](04-protocole-recherche.md)) : G1 backtest DEV → G2 robustesse (plateau, walk-forward, Monte Carlo) → G3 validation → G4 holdout → G5 paper (au moins 3 mois et 30 trades) → G5bis exécution en demo trading OKX (au moins 2 semaines, clé demo distincte, game day écrit) → G6 pilote live au palier 20 % (60 € alloués, au moins 3 mois et 30 trades, au plus 6 mois, ou clause d'échappement) → palier 50 % (150 €, 3 mois) → G7 12 mois à 100 % (300 €), qui commencent au premier ordre à 100 % (option (b), décision du 13/10 (18:35) ; règles de 04 §6). Chaque run et chaque fiche stratégie affichent la marche visée, les portes de cette marche en vert/rouge, et la suivante.
4. **Le mode est impossible à confondre.** Le badge de la barre du haut dit toujours « LIVE n · PAPER m ». LIVE a sa couleur propre et permanente, ambre plein avec texte sombre ; le rouge est réservé au danger immédiat (perte, alerte critique, commandes de coupure). **Paper et live ne sont jamais agrégés** : chaque position, ordre, alerte et ligne de journal porte son pill de mode ; les limites de risque se calculent sur le capital live seul, c'est-à-dire le capital alloué du palier en cours (60, 150 puis 300 €, convertis en USDC par la paire USDC-EUR ; seuils en USDC, 04 §6.2 ; décision du 13/10 (22:46)). Une instance en demo trading OKX (G5bis) n'est agrégée ni au paper ni au live.
5. **Deux commandes de risque, toujours visibles, toujours journalisées.** « Couper les entrées » (deux taps, la confirmation énonce le périmètre : « 3 bots dont 1 live ; 2 positions gardent leur stop ») et « Tout fermer au marché » (confirmation avec le coût estimé : positions, notionnel, taker 0,35 % + slippage, et saisie du mot FERMER). Les deux existent au niveau global, par bot, et la fermeture existe par position. Couper doit être facile, réarmer difficile : la levée se fait uniquement depuis Réglages, avec saisie de REARMER, motif obligatoire et rappel de la cause.
6. **UTC partout**, format `2026-10-13 14:32:05Z`, avec l'âge relatif à côté des horodatages d'alertes (« il y a 12 h »).
7. **Lisible sur téléphone pour les écrans d'opérateur.** Sur téléphone : un bandeau d'état pleine largeur sous la barre du haut (vert « OK · heure · maj il y a 3 s », orange ou rouge avec la cause la plus grave en clair et la réaction automatique), les confirmations de commande en bande pleine largeur avec des boutons de 44 px, et **les tableaux d'opérateur deviennent des cartes** (une par bot ou par position) ; les tableaux de chercheur restent des tableaux. Le menu bas donne accès aux sept écrans (« Plus » ouvre les quatre autres).
8. **Aucun secret affiché, jamais.** Une clé API apparaît comme « configurée le …, droits : lecture / trading » ; une clé avec droit de retrait est refusée à l'enregistrement.
9. **L'écran dit son âge et chaque commande dit son état.** « Mis à jour il y a N s » est affiché en permanence ; au-delà de 30 s, l'état passe en orange ; au-delà de 2 min ou sur erreur de connexion, la page est grisée avec un bandeau « HORS LIGNE depuis …, dernier état connu ». Toute commande passe par trois états visibles : envoi → confirmée par le serveur à HH:MM:SSZ → ou échec avec « Réessayer » et rappel des secours hors IHM : le fichier `var/kill` sur la machine, par SSH à travers le VPN, puis le quatrième étage du kill switch, l'application OKX (révoquer la clé ; pour fermer, annuler les algos puis vendre au marché depuis l'application, procédure du runbook, 08 §C.4), seul étage qui ne dépend ni de la box ni de la machine. L'état du kill switch affiché vient toujours du serveur, jamais du clic local.
10. **Accès par VPN seul, connexion obligatoire à l'intérieur** (décision du 13/10 (18:35), qui revient sur la version précédente de ce principe et de §7.1 : « l'IHM est sur Internet »). La machine est à la maison (§7.5) ; l'IHM n'est joignable qu'à travers un VPN WireGuard, un seul port UDP ouvert sur la box, aucune exposition HTTP ; le téléphone a son client WireGuard pour l'usage à 3 h du matin (§1). Login Symfony, 2FA TOTP (scheb/2fa-bundle), remember-me long sur le téléphone, CSRF sur tous les endpoints de commande et session liée au journal (« qui a coupé ») restent à l'intérieur du VPN, en défense en profondeur. Prérequis à vérifier depuis la box avant le paper : IP publique fixe ou DynDNS fiable, et pas de CGNAT ; en cas de CGNAT, il faut un tunnel sortant (à valider). Aucune notification sortante (décision du 2026-10-13 : pas d'alertes Telegram ni e-mail), donc aucun lien entrant à sécuriser ; les alertes se lisent dans la Vue d'ensemble et le Journal.

## 3. Écrans

| # | Écran | Rôle | Contenu principal | Jalon |
|---|---|---|---|---|
| 1 | **Vue d'ensemble** | Opérateur | Bandeau d'état et commandes, PnL du jour (net, réalisé + latent, en € et en % de la limite journalière), positions ouvertes avec « Fermer », alertes non acquittées, ligne « limites de risque », santé technique, courbe 90 jours en dernier | 3 |
| 2 | **Données** | Les deux | Matrice de couverture exchange × paire × timeframe avec trous signalés, jobs d'ingestion ; visualiseur de bougies (jalon 3, Lightweight Charts) | 2b |
| 3 | **Stratégies** | Chercheur | Catalogue : identifiant, version, hash de version, horizon, marche atteinte, essais ; fiche : frise temporelle (recherche / holdout verrouillé), schéma de paramètres, versions, bouton « Déverrouiller le holdout » (usage unique, journalisé), « Promouvoir » par marche avec le run de preuve | 2a |
| 4 | **Recherche** | Chercheur | Registre des hypothèses (manifeste, type exploratoire/confirmatoire, compteur d'essais), nouveau run, fiche de run, comparaison | 2a |
| 5 | **Exploitation** | Opérateur | Instances (stratégie@version, exchange, mode, capital alloué et palier 20/50/100 %, état explicite, PnL net, écart vs backtest), commandes par bot, positions avec localisation du stop, ordres, fills, jauges de limites, réconciliation exchange ↔ base, fiche d'instance | 3 |
| 6 | **Journal** | Les deux | Flux d'événements filtrable (type, bot, paire, dates), décisions avec leurs raisons, alertes critiques à acquitter | 2b |
| 7 | **Réglages** | Opérateur | Comptes exchange, grilles de frais, limites de risque, kill switch durable et réarmement | 3 |

Navigation : barre latérale fixe avec ces sept entrées ; barre du haut avec le badge de mode, l'état de santé (pill avec lettre, jamais un point seul), « maj il y a N s », l'horloge UTC et les deux commandes de risque. Les routes portent l'état (`/recherche/runs/{id}`, `/journal?type=decision&bot=…`), pagination côté serveur, pas d'état en JavaScript.

## 4. Détail des écrans de chercheur

### Un seul schéma temporel par stratégie

La fiche stratégie, le formulaire de run et la fiche de run affichent la même frise, avec les blocs du protocole : **DEV** 2018-01 → 2022-12 (exploration, walk-forward ancré, lectures illimitées mais comptées), **VALIDATION** 2023-03 → 2024-12 (au plus 3 lectures par trimestre), **HOLDOUT** 2025-03 → 2026-06 hachuré et verrouillé (1 lecture par trimestre, 3 à vie pour tout le projet), puis **FORWARD** à mesure que les dumps arrivent. Les dates d'exemple de la maquette (2022 → 2025-06, holdout 2025-07) sont à remplacer par ces blocs. Le formulaire de run ordinaire ne propose que la période de recherche ; la date du holdout y est grisée et non modifiable. Le holdout se consomme par une seule action, « Déverrouiller le holdout » sur la fiche stratégie, à usage unique, journalisée, qui crée le run confirmatoire ; la stratégie et ses versions suivantes portent ensuite le bandeau « holdout consommé le … par le run r-… ».

### Lancer un run

1. Hypothèse (une phrase, obligatoire).
2. **Type** : exploratoire (compte dans le budget d'essais, ne peut pas promouvoir) ou confirmatoire (une seule exécution, consomme le holdout, peut promouvoir).
3. Stratégie et version (hash de version affiché).
4. Paramètres générés depuis le schéma de la version (int, float, enum, bool, avec min/max/step).
5. Dataset (hash du snapshot) et paires filtrées par la couverture du dataset.
6. Frais : maker, taker, slippage en trois champs pré-remplis par la grille choisie (OKX 0,20/0,35 % par défaut), nom de la grille affiché. Un bandeau prévient quand l'exchange du dataset diffère de celui de la grille (dumps Kraken avec frais OKX).
7. Période de recherche et walk-forward (taille des fenêtres) ; holdout grisé.
8. « Pré-enregistrer et lancer » : le serveur gèle un **manifeste immuable** {hypothèse, version_hash, paramètres, dataset_hash, grille de frais, découpage, seuils de porte en vigueur, type}, affiche « pré-enregistré 2026-10-13 14:32:05Z · manifeste m-… » et incrémente le compteur d'essais. Les seuils de porte sont lus dans le manifeste à l'affichage, jamais dans les réglages courants.

### Lire un run

De haut en bas :

- **Verdict et marche** : « Franchit la marche 2 (walk-forward) ; prochaine : holdout, non déverrouillé ».
- **Lineage**, quatre identifiants toujours ensemble : `version_hash` (code), `config_hash` (code + paramètres + grille de frais), `dataset_hash`, `engine_version` ; ligne « Données : N bougies · trous · warm-up · bougies synthétisées » ; ligne « Reproductibilité : rejoué le …, identique à l'octet / DIFFÉRENT » et bouton « Rejouer ».
- **Portes** de la marche visée, trois colonnes : seuil, mesuré, **borne basse à 90 %** (bootstrap sur les trades) ; la porte passe sur la borne basse. Une ligne « seuil corrigé pour N essais » avec N visible (la formule est dans le protocole, l'IHM l'affiche). L'unité qui fait foi (R ou %) est en tête du tableau ; la porte « vs buy & hold » a une unité explicite (rendement/drawdown net de frais).
- **Equity hors échantillon seulement** : concaténation des fenêtres de test, trait vertical à chaque frontière de pli, frise des plis au-dessus (une cellule par pli, signe du résultat), références en gris : buy & hold net des mêmes frais, et médiane de la distribution de contrôle à entrées aléatoires (jalon 2). Drawdown en dessous, un axe par graphique.
- **Plis du walk-forward** : entraînement (fenêtre, trades, expectancy, PF) | test (idem) | ratio test/entraînement | paramètres retenus ; synthèse « WFE médiane · N plis dont les paramètres optimaux ont changé ». Un pli dont la fenêtre contient un trou de données est marqué.
- **Distribution** : histogramme des PnL nets en R (médiane et moyenne), nuage MFE vs MAE coloré par résultat, « PnL sans les N meilleurs trades » pour N = 1, 3, 5, 10 (porte : reste positif sans les 5 meilleurs), petits tableaux par année, raison de sortie, pli, paire.
- **Pourquoi pas de trade** : bougies bloquées par règle.
- **Trades** : entrée, sortie, prix, durée, frais en devise, slippage modélisé, PnL net, R, MFE, MAE, pli, raison de sortie ; triable, paginé, export CSV.

### Comparer

Deux à trois runs côte à côte (quatre quand une quatrième couleur de série sera validée). Les lignes de lineage ouvrent le tableau et chaque ligne qui diffère est surlignée ; un `dataset_hash` différent déclenche un avertissement, pas un refus : « datasets différents : comparaison de biais, pas de paramètres » (c'est la mesure Kraken/OKX demandée par la matrice des exchanges). Quand les runs ne diffèrent que par les paramètres, l'étiquette « balayage de paramètres, N cellules » apparaît et chaque cellule compte comme un essai. Equity normalisées à 100 sur un même graphique.

## 5. Technologie

| Option | Pour | Contre | Décision |
|---|---|---|---|
| **A. Symfony + Twig + Turbo + Stimulus, Live Components et Mercure limités** | Même stack que le control plane, rendu serveur, tests WebTestCase, ton cœur de compétence | Interactions riches à confier à une bibliothèque de graphiques | **Retenue** |
| B. SPA React/Vue + API Platform | Interactions riches | Second projet à maintenir seul ; V3 a fini avec Twig et React sans trancher | Écartée |
| C. Application de bureau (Tauri) | Local, rapide | Pas de téléphone | Écartée |

Règles fixées, sans option ouverte :

- **Live Components** uniquement pour les formulaires à champs dépendants (stratégie → version → paramètres).
- **Turbo Frames avec rafraîchissement périodique (5 à 10 s)** pour toute lecture au jalon 2, y compris la progression d'un run (écrite en base par le moteur).
- **Mercure + Turbo Streams** seulement au jalon 3, pour trois flux : positions et fills, santé, journal. Pas de Mercure dans le docker-compose avant.
- **Graphiques** : tout graphique à axe temporel passe par **TradingView Lightweight Charts** (equity en LineSeries, drawdown en BaselineSeries, bougies en CandlestickSeries, entrées, sorties et stops en marqueurs ▲/▼, crosshair, tactile et zoom inclus). Le SVG est réservé aux sparklines statiques et aux barres. Les plis du walk-forward sont une frise HTML au-dessus du graphique, pas des rectangles superposés. Un graphique = une mesure, jamais de double axe.
- **Couleurs** : palette validée par le validateur de contraste du skill dataviz, variantes texte plus sombres en thème clair, redondance de forme partout (bougies creuses/pleines, ▲/▼, lettre dans le pill de santé, texte sur les barres de sévérité).
- **Accès et authentification** : principe 10 (VPN seul, login + TOTP à l'intérieur).
- **Tests** : WebTestCase sur les pages et les endpoints de commande ; pas de tests navigateur.

Ce que l'IHM exige du control plane (à reporter dans 03-architecture.md) : API de lecture pour chaque tableau (runs, trades, plis, distributions, événements, couverture), **endpoints de commande idempotents qui renvoient un accusé horodaté**, flux d'événements au jalon 3, identité d'utilisateur et journal d'audit, vérification toutes les 30 à 60 s que chaque stop existe encore sur l'exchange, avec la bonne taille (08 §C.6), réconciliation exchange ↔ base.

## 6. Calendrier

| Jalon | Contenu IHM | Ordre de grandeur |
|---|---|---|
| **1** (moteur) | Aucune IHM. Le moteur émet JSON/CSV (manifeste, métriques, trades, plis, distributions) ; un rapport HTML statique reprend les sections de la fiche de run, Distribution comprise. Qui le rend : voir §7, décision 3 | — |
| **2a** | Stratégies + Recherche (registre, nouveau run, fiche de run, comparaison) en pages Twig, progression par Turbo Frame ; `/recherche` est la page d'accueil | 12 à 18 jours |
| **2b** | Données (matrice de couverture seule) + Journal (filtres, pagination) | 4 à 6 jours |
| **3** | Vue d'ensemble, Exploitation (fiche d'instance, positions avec stop, réconciliation), Réglages (kill switch, réarmement), Mercure, bandeau mobile, cartes mobiles, visualiseur de bougies | 15 à 25 jours |

Les estimations sont des ordres de grandeur pour un développeur seul maîtrisant Symfony ; à valider une fois le control plane posé. Les marches, quotas et seuils affichés viennent du protocole (04) ; l'IHM les montre, elle ne les définit pas.

## 7. Décisions du 2026-10-13 (réponses de Haythem)

1. **Un seul utilisateur.** Pas de rôles, pas de second compte, pas de journal nominatif. La connexion avec 2FA reste ; depuis la décision du 13/10 (18:35), elle se fait à l'intérieur du VPN (point 4). Le journal note l'origine de chaque commande.
2. **Pas d'alertes.** Aucune notification sortante, ni Telegram ni e-mail. Les alertes existent dans l'IHM (Vue d'ensemble, Journal, acquittement) et c'est là qu'on les lit. Le secours quand l'IHM est injoignable est le fichier `var/kill` sur la machine, par SSH à travers le VPN, et en dernier recours l'application OKX (principe 9). Le rapport quotidien est généré dans le Journal, pas envoyé.
3. **Rapport statique au jalon 1 : oui.** Le moteur Rust émet JSON/CSV ; le rapport HTML est rendu par un squelette Symfony minimal (commande console + templates Twig réutilisés par la fiche de run du jalon 2a). Le rapport jetable côté moteur (templating Rust) est écarté (03 §3).
4. **Accès à l'IHM par VPN seul** (décision du 13/10 (18:35), question 9 de 08 §C.7). Revient explicitement sur §7.1 et §2.10 dans leur version précédente (« l'IHM est sur Internet ») : WireGuard, un seul port UDP ouvert, aucune exposition HTTP ; login + TOTP conservés à l'intérieur du VPN. IP fixe ou DynDNS fiable et absence de CGNAT sont des vérifications, pas des décisions (principe 10).
5. **Machine dédiée à la maison** (décision du 13/10 (18:35), question 8) : pour le paper et le live, le moteur, le control plane et l'IHM tournent sur un mini-PC x86 ou un Raspberry Pi 5 avec SSD, jamais sur le poste de développement (08 §C.2). VPS seulement au-delà de ≈ 2 000 € ou si la connexion coupe plus de 15 min plus d'une fois par mois pendant le paper.
6. **Cycle de vie et pause journalière** (décisions du 13/10 (18:35), questions 3 et 5) : les marches affichées sont celles du principe 3 (G5bis en demo, puis paliers 20 %, 50 % et 100 %, option (b)) ; la pause journalière à −2 % reprend seule au jour UTC suivant, REARMER reste pour le CUSUM, l'expectancy et les causes critiques.

## 8. Amendements reportés au jalon concerné

Issus de la critique, intégrés dans la maquette quand ils étaient peu coûteux, à reprendre lors de l'implémentation :

**Jalon 2**
- Routes avec l'état dans l'URL, pagination serveur, filtres du journal (bot, paire, dates).
- Formulaire de run : FormType natifs depuis le schéma (int, float, enum, bool), paires filtrées par le dataset, frais en trois champs, ligne « fenêtre × paires = N bougies, ~X s », erreurs standard.
- États de chaque écran : vide avec action proposée, chargement, erreur, périmé ; vue « run en cours » avec progression, log moteur et « Annuler » ; verdict « échoué : raison ».
- Panneau « Voisinage des paramètres » (heatmap 2D, sensibilité 1D), porte « plateau » quantifiée ; balayage de paramètres compté comme N essais.
- Références buy & hold et distribution de contrôle sur l'equity, histogramme gris avec rang (porte ≥ 95e percentile).
- Échelle des marches du principe 3 en tête de run et de fiche stratégie, portes groupées par marche.
- Ligne « Données » sous l'en-tête de run, plis avec trou marqués.
- Devise de référence et conversion horodatée.

**Jalon 3**
- Vue d'ensemble réordonnée pour l'opérateur (bandeau, PnL du jour, positions, alertes, limites, santé, courbe).
- Positions : prix actuel, distance au stop, localisation du stop (« exchange #algo-… vérifié à … » ou « local, non protégé » en rouge), pill de mode, « Fermer » ; vérification des stops côté exchange toutes les 30 à 60 s (existence et taille de l'algo, 08 §C.6).
- États de bot explicites et partagés (en marche, en pause données, bloqué limite (pause journalière à −2 %, reprise automatique au jour UTC suivant), arrêté par qui, erreur, sans battement) ; trois latences définies avec seuil et réaction automatique.
- Réconciliation exchange ↔ base affichée, écart = alerte critique bloquante.
- « Promouvoir » par marche avec run de preuve ; fiche d'instance paper (backtest-ombre quotidien, réel vs ombre, signaux pris/manqués/expirés, taux maker, slippage réalisé, portes de passage en live) ; fiche d'instance demo (G5bis) avec le game day écrit : scénario, réaction attendue, résultat consigné.
- Alertes à acquitter, compteur de non acquittées, âge relatif.

**Idées gardées (P3)** : equity en % ou log par défaut, expectancy en R et en % côte à côte ; petits multiples par pli ; quatrième couleur de série validée ; heure locale au survol ; préférences locales en localStorage (jamais d'état de commande) ; balisage Twig (liens sur les lignes, vraies tables, aria).

**Écartés** : passkey et liens magiques (vecteur d'attaque), feuille mobile dédiée au kill (bande sous la barre suffit), grisage à 10 s (clignoterait avec le polling), LIVE en rouge (plus rien ne serait rouge sous stress), refus de comparer deux datasets (c'est la mesure de biais), tests navigateur, formule de correction dans l'IHM (appartient au protocole), visualiseur de bougies et Vue d'ensemble au jalon 2 (coquilles vides avant le paper).
