# tradingV4 — Ce qui manque aux bots de trading du marché

Date : 2026-10-13. Question de Haythem : « qu'est-ce qui manque aux bots qui existent maintenant sur le marché ? » Lecture orientée vers notre cas (spot crypto, utilisateur unique, résident FR, OKX spot 0,20/0,35 %, objectif PnL net positif). Les documents 01 à 05 ont été relus pour ne pas répéter ce qui est déjà décidé ; chaque lacune indique si elle est **déjà couverte** par notre conception ou **à ajouter**.

## 1. Synthèse

Le marché a commoditisé l'**outillage** (connecteurs, indicateurs, DCA, grid, dashboards, copy trading). Ce qui manque est presque entièrement du côté de la **preuve** : savoir si une stratégie gagne réellement de l'argent net, après coûts réels, hors échantillon, et le détecter vite quand elle cesse de gagner. Aucun produit grand public ne vend cela, parce que la réponse honnête est le plus souvent « non ».

Conséquence pour tradingV4 : la valeur n'est pas dans le moteur ou les connecteurs (Freqtrade gère déjà OKX spot avec stop natif sur l'exchange), mais dans le protocole de recherche (04) et la mesure backtest ↔ paper ↔ live. Notre conception couvre déjà la majorité des lacunes ; trois restent à ajouter (§3).

## 2. Lacunes constatées, et où on en est

| # | Lacune du marché | Constat (source) | tradingV4 |
|---|---|---|---|
| 1 | **Fills optimistes en backtest** | Freqtrade : ordres remplis au prix demandé dès que le prix est dans le range de la bougie, **aucun slippage modélisé** ; l'ordre intra-bougie est inconnu (option `--timeframe-detail` pour l'atténuer) ([doc backtesting](https://www.freqtrade.io/en/stable/backtesting/)) | **Couvert** : règles de fill pessimistes, stop au pire de (niveau, open 1m), stop+target même bougie = stop, slippage de stop en distribution (04 §3.1), spread mesuré sur le carnet OKX (04 §3.2) |
| 2 | **Contraintes d'exchange historiques ignorées** | Freqtrade applique les limites et précisions **actuelles** aux prix historiques, ce qui fausse les tailles minimales ([doc backtesting](https://www.freqtrade.io/en/stable/backtesting/)) | **À ajouter** (§3.2) : rien dans 03/04 sur min notional / lot size / tick size dans le simulateur |
| 3 | **Sur-optimisation encouragée** | Les hyperopts / optimiseurs intégrés cherchent le meilleur jeu de paramètres sans compter les essais ; le meilleur de N essais a un Sharpe gonflé mécaniquement ([Bailey, Borwein, López de Prado, Zhu, *The Probability of Backtest Overfitting*](https://papers.ssrn.com/abstract=2326253) ; [Bailey & López de Prado, *The Deflated Sharpe Ratio*](https://papers.ssrn.com/abstract=2460551)) | **Couvert** : pré-enregistrement, registre `trials.csv` append-only, Sharpe de bruit sur chaque rapport, contrôles aléatoires meilleur-de-N, holdout 3 lectures à vie (04 §4-5) |
| 4 | **Détection de look-ahead partielle** | `lookahead-analysis` de Freqtrade ne vérifie pas les signaux jamais déclenchés (faux négatifs) et force les ordres market ([doc](https://www.freqtrade.io/en/stable/lookahead-analysis/)) | **Couvert par construction** : simulateur événementiel unique, même code backtest/paper/live, G0bis stratégie aléatoire (04 §8). Le test « décalage d'une bougie » en Monte Carlo (04 §9.9) sert de garde-fou |
| 5 | **Pas de mesure backtest ↔ live** | Les plateformes affichent un PnL live et un backtest, jamais l'écart trade par trade ni sa décomposition (frais, slippage, signaux manqués). Freqtrade dit seulement que le backtest « ne remplace pas le dry-run » | **Couvert** : rejeu hebdomadaire du backtest sur la semaine de paper (≥ 90 % des signaux), pass G6 « frais + slippage réalisés ≤ modélisés + 25 % » (04 §6) |
| 6 | **Pas de benchmark honnête** | PnL affiché seul, sans buy-and-hold ni exposition égale ; un bot long-only en bull « gagne » sans edge | **Couvert** : cash, BTC B&H, 50/50, benchmark à exposition égale, verdict « bêta filtré » (04 §1). Question Q3 toujours ouverte |
| 7 | **Garde des clés API par un tiers** | 3Commas : ~100 000 clés API publiées fin 2022, ~22 M$ de pertes rapportées, fuite confirmée par le CEO ([SiliconANGLE](https://siliconangle.com/2022/12/29/crypto-trading-service-3commas-confirms-massive-api-key-leak-hack/)) | **Couvert en partie** : auto-hébergé, clé sans droit de retrait (04 §6.2). **À ajouter** : restriction IP de la clé OKX et stockage chiffré (§3.3) |
| 8 | **Pérennité / dépendance fournisseur** | Fermetures en série : Tuned (11/2022), NapBots (01/2023), Shrimpy grand public (07/2023), Trality (07/2023), BitUniverse (07/2023) ([HaasOnline, 07/2023](https://haasonline.com/blog/the-fall-of-more-trading-bot-companies-and-projects)). Jesse : le live exige une licence payante ([doc](https://docs.jesse.trade/docs/livetrade)) | **Couvert** : code et données à nous (dumps Kraken, Parquet hashés) |
| 9 | **Bots « plateforme-exchange »** | Pionex (MiCA via Pionew Ireland, autorisé pour la France au 18/12/2025 selon la [liste blanche AMF](https://www.amf-france.org/fr/espace-epargnants/proteger-son-epargne/listes-blanches/psanpsca/pionew-ireland-limited)) : bots grid/DCA intégrés à frais bas, mais stratégies fermées, pas de recherche ni de backtest propre | Hors sujet : on ne cherche pas un grid packagé. À garder en tête comme **lieu d'exécution** alternatif si les frais OKX tuent tout (à valider : grille de frais Pionex EU, API spot) |
| 10 | **Market making réservé aux gros volumes** | Hummingbot gère OKX spot en `LIMIT`/`LIMIT_MAKER` ; OKX est partenaire de la fondation (part des frais reversée) ([doc OKX](https://hummingbot.org/exchanges/okx/)). À 0,20 % maker sans rebate, le market making retail n'a pas de marge | Hors sujet, cohérent avec 04 (pas de scalping sur OKX) |
| 11 | **Fiscalité française absente** | Aucun bot ne produit un journal de cessions au format FR. En 2026 : PFU **31,4 %** (12,8 % IR + 18,6 % prélèvements sociaux), plus-value au prix moyen pondéré du portefeuille, échanges crypto-crypto (stablecoins compris) en sursis d'imposition, seule la cession contre euros est imposée ; risque de requalification en BNC si l'activité est jugée habituelle ([Ramify](https://www.ramify.fr/crypto/fiscalite), [Finary](https://finary.com/fr/blog/crypto/investir/fiscalite-cryptomonnaie)) | **À ajouter** (§3.1) : rien dans 01-05 |
| 12 | **Pas de détection de décrochage** | Les bots s'arrêtent sur un drawdown fixe ou jamais ; aucun ne compare la série live à la distribution attendue du backtest | **Couvert** : arrêts durs DD 15 %/20 %, slippage > 2× modélisé, re-validation trimestrielle sur dump Kraken, expectancy OOS glissante (04 §6.3) |

## 3. Ce que je propose d'ajouter à la conception

### 3.1 Journal fiscal (impact PnL et jalon 2)

- Le PnL « net » de 04 §1.1 est **avant impôt**. Un PnL net de 10 % devient environ 6,9 % après PFU sur la part convertie en euros. Le verdict ne change pas de signe, mais le benchmark Livret A (net d'impôt) devient relativement plus fort. **Tranché le 13/10 (18:35)** (décision 7 de 08 §B.6) : verdict avant impôt, Livret A net d'impôt en face.
- Le moteur reste en USDC (décision du 13/10 (22:46)) ; tant qu'on ne repasse pas en EUR (paire USDC-EUR), les échanges crypto ↔ USDC ne sont pas un fait générateur (règle générale, neutralité fiscale **à valider** pour le cas précis de l'USDC sous MiCA).
- Exigence moteur : un export des **cessions contre EUR** avec valeur globale du portefeuille à chaque cession (nécessaire au calcul PAMP du formulaire 2086). Coût faible si le journal de trades (04 §9.3) garde la valorisation du portefeuille à chaque événement.
- Risque BNC : un bot qui trade toute l'année peut être vu comme activité habituelle. Ce n'est pas tranché par la volumétrie seule (faisceau d'indices). **À valider** avec un conseiller fiscal avant G6, pas avant.

### 3.2 Contraintes d'exchange dans le simulateur (jalon 1)

Ajouter à `simulator` (03 §2) : min notional, lot size et tick size par paire, chargés depuis `GET /api/v5/account/instruments` sur `eea.okx.com` (clé en lecture seule : l'API publique des instruments n'est pas filtrée par région, 02 §8) et **versionnés avec le dataset** (snapshot daté dans le `dataset_hash`). Un ordre sous le minimum est rejeté et compté, comme les rejets de cash existants (04 §3.1 point 5). Enjeu concret à petit capital : avec 8 positions simultanées et un sizing par risque, certaines paires tombent sous le minimum ; le backtest doit le voir.

### 3.3 Sécurité de la clé live (jalon G6)

En plus de « sans droit de retrait » : clé OKX **liée à l'IP publique de la box, obligatoire** (la machine dédiée est à la maison, décision du 13/10 (18:35) ; 08 §C.5). Dérogation seulement si l'IP de la box se révèle non fixe pendant le paper (mesurée dans le Journal) : la liaison est alors remplacée par un canari horaire `GET /account/balance` et une alerte en cas de refus ; OKX supprime une clé de trading non liée à une IP après 14 jours d'inactivité (02 §8). Secret chiffré au repos (pas dans `.env` versionné), clé distincte par environnement, rotation documentée. Rien de neuf techniquement, mais c'est la leçon 3Commas et ce n'est pas écrit dans 04 §6.2.

## 4. Ce qu'il ne faut pas reconstruire

- Connecteurs multi-exchanges génériques : un seul exchange en live (OKX), CCXT ou équivalent en référence si besoin.
- Bots DCA / grid / copy trading / marketplace de signaux : ce sont les produits du marché, et ils n'ont pas d'edge démontré.
- Dashboard riche : l'IHM 05 suffit.
- Note honnête : pour la seule **exécution** sur OKX spot, Freqtrade sait déjà placer un stop natif sur l'exchange en market ou limit ([doc stoploss](https://www.freqtrade.io/en/stable/stoploss/)). Le choix Rust est acté ; ce point sert seulement à rappeler que l'effort doit aller au protocole et à la mesure, pas à refaire de l'exécution.

## 5. Vérifications effectuées

- Docs Freqtrade (backtesting, lookahead-analysis, stoploss), Hummingbot (connecteur OKX), Jesse (live trade) lues le 2026-10-13.
- Liste blanche AMF pour Pionew Ireland lue le 2026-10-13.
- Taux PFU 31,4 % : Ramify (l'article cite aussi 30 % par endroits, incohérence notée) ; **vérifié le 13/10** sur [impots.gouv.fr](https://www.impots.gouv.fr/particulier/questions/comment-declarer-les-plus-ou-moins-values-sur-cessions-dactifs-numeriques) (page modifiée le 17/07/2026) : PFU de 31,4 % (12,8 % d'impôt + 18,6 % de prélèvements sociaux), « les opérations d'échange sans soulte d'actifs numériques bénéficient d'un sursis d'imposition » ; la page d'economie.gouv.fr du 02/10/2025 affiche encore 30 %, elle est périmée.
- Non vérifié : statut MiCA actuel de 3Commas, Cryptohopper, Gunbot pour un résident FR (ce sont des logiciels, pas des PSCA ; sans incidence sur notre choix d'auto-héberger).
