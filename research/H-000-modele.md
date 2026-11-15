# Modèle de fiche de pré-enregistrement

Copier ce modèle sous `research/H-NNN.md`, remplacer le numéro et renseigner
chaque champ avant le premier run DEV. H-000 est un modèle et ne peut pas servir
de fiche exécutable. Une ligne par champ ; conserver les libellés.
La fiche doit être commitée avant son utilisation.

```text
H-NNN            : numéro d'essai (compteur global, jamais remis à zéro)
Date / auteur    :
Famille          : F0 régime seul (témoin, jamais candidate) | F1 cassure Donchian + filtre SMA200 | F2 croisement de MM | F3 momentum cross-sectionnel mensuel (documentée, rejetée par G0, 08 §A.6) | F4 compression puis cassure de volatilité | F5 pullback EMA en tendance 4h | autre (≤ 5 familles par cycle, F0 en plus)
Hypothèse économique : 3 phrases max — pourquoi quelqu'un paierait ce trade (persistance de tendance, sur-réaction, saisonnalité…)
Règle d'entrée   : ≤ 5 lignes, ≤ 3 conditions dont le filtre BTC > SMA200 (§4.2)
Règle de sortie  : stop initial k × ATR14, trailing, time-stop 30 j (F3 : rééquilibrage ≤ 31 j et `roll`, §4.2) ; le filtre de régime ne ferme pas, sauf F0
Paramètres       : ≤ 4, chacun avec valeur a priori et grille (≤ 27 variantes au total, énumérées)
Timeframe        : 1d | 4h (| 1h maker seulement)
Univers          : règle §2.2 + liste figée des paires OKX 2026 pour validation/holdout ; liste live (paper et paliers 20 %, 50 %, 100 %) fixée une fois par la règle d'exécutabilité (§5 G0) au notional du palier 100 %
Profil de coûts  : grille (okx_regular / bitvavo_usdc), spreads mesurés ou défauts par paire, et ratio coût/stop calculé (≤ 0,10 R en 1d, ≤ 0,15 R en 4h maker)
Faisabilité G0   : (1) coût AR et coût/stop ; (2) échantillon attendu = fréquence × paires × années ≥ 1,5 × n_min (marge sur l'aléa du nombre de trades) ; (3) sizing sans levier : notional = risque / stop ≤ cash libre ; positions à plein risque = floor(stop / 1 %) ; à 100 % des 300 €, ≈ 35 € par ligne et 8 lignes à stop 8,5 % (1d), 75 € par ligne et 4 lignes à stop 4 % (4h maker) ; (4) pour chaque paire de la liste live, 0,5 % × volume quote 1m médian ≥ notional (règle §3.1 point 4) et notional ≥ minSz × prix (règle unique de §5 G0), sinon la paire sort de la liste live
n_min / μ_cible  : n_min = max(200 ; (2 × σ_attendu / μ_cible)²) ; ex. μ 0,15 R, σ 1,25 → 278 ; μ 0,20 R → 200 ; σ 1,5 → 400
Signaux attendus : par paire-mois (1d : 0,2 à 2 ; 4h : 0,5 à 5), comptés sur 2 paires avant gel
Contrôle         : meilleur de N tirages aléatoires à régime égal, N = taille de la grille, ≥ 200 graines (1 000 cible ; 20 = smoke test seulement) ; contrôle apparié par config (même géométrie) ; contrôle sans filtre
Règle d'abandon  : écrite d'avance, ex. « si la médiane des 9 variantes a μ_R < 0 en baseline, la famille est fermée, pas retouchée »
Connaissances antérieures : résultats V3 sur la même famille, périodes 2023-2026 déjà vues
Claim holdout    : non | oui, pour la config <config_hash>, inscrit AVANT lecture
dataset_hash     : figé ; commit du moteur : <sha> (un résultat produit par un autre commit est invalide)
```

À la sélection du candidat, commiter son fichier `research/frozen/H-NNN.json`
puis créer le tag `frozen/H-NNN` couvrant les deux fichiers. Le claim HOLDOUT
s’inscrit au registre par la CLI avant la lecture, indépendamment du texte ici.
Le contrat complet est décrit dans [S12](../docs/specs/s12.md).
