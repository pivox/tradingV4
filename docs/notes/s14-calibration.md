# S14 — protocole synthétique de calibration

Ce protocole est fixé avant la campagne de test. Il concerne exclusivement des
fixtures techniques ; aucun rendement de marché réel ni verdict G1 n'en découle.
Les résultats seront ajoutés après exécution du moteur et conservés avec les logs.

## Loi nulle et exécution

400 marchés indépendants, N=3 géométries exogènes à durées fixes déclarées
3/5/7 slots, K=200. Les calendriers source sont planifiés indépendamment des prix
sur le même support compatible que les contrôles ; ils sont causaux lors du rejeu.
La durée en position et l'occupation totale, comprenant les délais natifs, sont
distinguées. Le vrai Book applique les fills, instruments, cash et coûts identiques
aux observations et aux contrôles. Vérifier avant tout résultat que les stops
légaux ne sont jamais touchés, le cash non liant et les quotas remplis/conservés
exacts ; aucune correction après coup ni exclusion d'une réplication sur son PnL.

Chaque marché compte environ 1 000 bougies 1m, prix initial 100 et incréments
indépendants de signe symétrique et d'amplitude dans [0,00005;0,0001]. Leur
amplitude continue évite une grille de scores dégénérée ; elle est indépendante
du signe. Les clôtures restent analytiquement dans [99,9;100,1] ; les mèches
compatibles et les prix de fill sont aussi vérifiés par le test. q=20 lots
conservés par géométrie, sans filtre BTC.

Graines maîtres fixes : marché 0x53313401, source 0x53313402, contrôles 0x53313403.
Chaque marché reçoit trois sous-flux dérivés de sa graine maîtresse et de son
index explicite avant génération des prix. Les géométries ont leurs propres
identités ; aucun seed n'est choisi selon le nombre de rejets observé. Publier
la dérivation exacte et les identités avec le résultat du test.

## Critère fixé avant mesure

Score : moyenne des R nets réellement conservés. Pour chaque marché, comparer
le maximum des N observations aux K maxima de N contrôles indépendants.
`p=(1+b)/(K+1)`, b comptant les égalités comme dépassements. Sans égalités,
la probabilité du test de rang à 5 % est `floor(0,05*201)/201 = 10/201`.
La bande de recette préfixée est **8 à 35 rejets inclus sur 400**, quantiles
binomiaux 0,001 et 0,999 ; sa masse est 0,9988116804393153.

Le test exige l'absence d'égalité dans ces comparaisons. Un test séparé à scores
constants doit donner p=1 ; les égalités ne justifient pas une tolérance ajustée
après lecture des résultats. Cette calibration valide ce modèle exogène
échangeable et son implémentation, sans promesse d'alpha universelle pour toute
stratégie sans edge ou toute géométrie conditionnée aux prix futurs.

## Sensibilité distincte

40 marchés supplémentaires, N=1, K=200, avec un état exogène annoncé au début
de blocs de 20 pas, signes +/− alternés et drift constant 0,02*signe pendant
chaque bloc, plus bruit symétrique indépendant 0,0001. La stratégie causale
entre en bloc positif assez tôt pour que délai de fill et durée entière restent
dans le bloc  ; 25 blocs positifs sur 1 000 pas rendent q=20 possible avant observation
des prix. Les contrôles remplacent ce déclencheur avec les mêmes quotas et coûts.
Critère préfixé : au moins 32/40 rejets à p<=0,05. Cette mesure est séparée du test
d'alpha ; aucune adaptation de la bande précédente à partir de sa puissance.

## Commande et preuves

La campagne dédiée sera exécutée par le test natif :

```bash
cargo test --release --locked --test controls_calibration -- --ignored --nocapture
```

Publier : nombre de rejets et égalités, contrôle des quotas/risques/cash, graines
et sous-flux, nombre de Books/sondes/rejeux acceptés et tentés, commande, moteur,
matériel, temps et logs. La présente note fixe le protocole ; aucune de ces deux
campagnes n'a encore été exécutée à ce point du développement.
