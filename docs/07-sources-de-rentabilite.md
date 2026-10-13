# tradingV4 — Pourquoi certains bots ont un PnL positif

Date : 2026-10-13. Question de Haythem : « pourquoi les bots arrivent à avoir un PnL positif ? » Complète 06 (qui traitait de l'outillage). Lecture orientée vers notre cas : spot, OKX EEE à 0,20/0,35 % (aller-retour taker ≈ 0,72-0,85 % avec spread, cf. 04 §3), capital de 300 € au maximum (décision du 13/10 ; le texte ci-dessous raisonne en % et en R, le capital est un paramètre hors stratégie).

## 1. Réponse courte

Les bots qui gagnent durablement ne gagnent presque jamais grâce à un signal d'indicateur. Ils encaissent une **prime structurelle** que quelqu'un d'autre paie :

1. le **spread** payé par ceux qui veulent exécuter tout de suite (market making) ;
2. les **écarts de prix** entre places, capturés par les plus rapides (arbitrage, latence) ;
3. la **prime de financement** payée par les acheteurs à levier (carry / funding) ;
4. la **persistance des tendances**, due aux flux lents et à l'attention des particuliers (trend following / momentum).

Les trois premières exigent des frais quasi nuls, de la vitesse ou des dérivés. Seule la quatrième survit à nos frais. Le reste du « PnL positif » affiché sur le marché est du **bêta** (le marché a monté) ou du **marketing** (sélection des résultats montrés).

## 2. Les sources d'edge, une par une

| Source | Comment ça gagne | Ordre de grandeur | Ce qu'il faut | Accessible à tradingV4 ? |
|---|---|---|---|---|
| **Market making** | Poser des ordres des deux côtés, gagner le demi-spread sur chaque aller-retour, plus d'éventuelles remises maker | Quelques bp par aller-retour, sur un volume énorme | Frais maker ≈ 0 ou négatifs. Chez OKX, VIP 1 = 0,045 % / 0,05 % à partir de 5 M$ de volume spot sur 30 jours, VIP 5 = 0 % / 0,03 % à 200 M$ ([ForkLog, 07/2025](https://forklog.com/en/how-to-save-up-to-3000-a-month-on-fees-comparing-the-vip-programmes-of-okx-bybit-bitget-htx-and-mexc/), grille globale, pas EEE) ; gestion fine de l'inventaire et de l'adverse selection | **Non** : notre maker à 0,20 % vaut environ 40 fois le spread des majors (0,5 bp, 04 §3) |
| **Arbitrage / latence** | Racheter là où c'est moins cher, revendre là où c'est plus cher ; ou réagir avant les autres à une information | Quelques bp, en concurrence avec des firmes colocalisées | Capital déposé sur plusieurs places, latence en millisecondes, frais minimes | **Non** : chaque jambe coûte 35 bp, l'écart à capturer est de l'ordre du bp |
| **Carry / funding** (long spot + short futures ou perp) | Encaisser ce que paient les acheteurs à levier pour garder leur position | **> 10 %/an en moyenne, jusqu'à ~60 %**, très variable ; un carry élevé précède souvent les krachs, et le risque est la liquidation de la jambe short pendant les pics ([BIS WP 1087, *Crypto carry*, 2023](https://www.bis.org/publ/work1087.pdf)) | Une jambe dérivés (short) | **Hors périmètre** (spot uniquement, réaffirmé le 13/10 (18:35) : pas de compte dérivés). Chez OKX EEE, l'accès aux perps passerait par le compte X-Perps (04 §3, à valider), que Haythem refuse d'ouvrir. C'est la source structurelle la mieux documentée accessible à un particulier |
| **Tendance / momentum** | Rester exposé quand le marché monte, sortir quand il baisse ; les rendements crypto ont un fort momentum temporel ([Liu & Tsyvinski, *Risks and Returns of Cryptocurrency*, RFS 2021](https://papers.ssrn.com/abstract=3226806)) | Portefeuille rotatif sur le top 20, depuis 2015 : Sharpe > 1,5 net de frais, alpha 10,8 %/an contre BTC ([Zarattini, Pagani, Barbon, *Catching Crypto Trends*](https://concretumgroup.com/catching-crypto-trends-a-tactical-approach-for-bitcoin-and-altcoins/)) | Peu de trades (2 à 20/an/paire), donc des frais supportables ; tolérance à de longs drawdowns | **Oui** : c'est la voie de 04 (1d et 4h). Notre propre mesure, sans porte, sur 8 variantes simples BTC 1d : CAGR 15-36 % sur 2018-2026 contre 22 % en buy-and-hold, années baissières médiane ≈ −30 % (04 §7). L'edge existe surtout en **diversification** sur plusieurs paires et en **coupure des bears**, pas en battant BTC à coup sûr |

## 3. D'où vient le reste du « PnL positif » affiché

- **Bêta déguisé.** Un bot long-only (DCA, grid, « smart trade ») gagne mécaniquement quand le marché monte. Sans benchmark à exposition égale, impossible de distinguer l'edge du bull. Notre protocole le teste déjà (04 §1, verdict « bêta filtré »).
- **Grid bots** (inférence, pas de source chiffrée) : le « profit de grille » affiché est réalisé à chaque aller-retour, tandis que l'inventaire racheté pendant une baisse reste en perte latente. En range, il gagne ; en tendance baissière, il accumule l'actif qui baisse ; en tendance haussière, il vend trop tôt et sous-performe le buy-and-hold.
- **Sélection des résultats montrés.** Marketplaces de stratégies et classements de copy trading montrent les gagnants du moment parmi des milliers d'essais. C'est le problème du meilleur-de-N ([Bailey et al., PBO](https://papers.ssrn.com/abstract=2326253)), à l'échelle d'une plateforme.
- **Le vendeur gagne, pas forcément l'utilisateur.** Le revenu d'un 3Commas ou Cryptohopper est l'abonnement ; celui de Pionex, les frais d'exchange. Leur modèle ne dépend pas du PnL de leurs clients.
- **Réalité de base chez les particuliers.** Au Brésil, parmi ceux qui ont fait du day trading sur futures plus de 300 jours, **97 % ont perdu de l'argent et 0,4 % ont gagné plus qu'un guichetier de banque (54 $/jour)** ([Chague, De-Losso, Giovannetti, *Day Trading for a Living?*](https://ideas.repec.org/p/fgv/eesptd/525.html)). Ce n'est pas de la crypto, mais c'est la meilleure mesure disponible de ce que donne le trading court terme discrétionnaire ou outillé chez un particulier.

## 4. Ce que ça implique pour tradingV4

1. **Nos frais décident de la voie, pas nos préférences.** À ≈ 0,8 % l'aller-retour, seule la tendance basse fréquence est jouable. Cela confirme le choix de 04 (1d, 4h ; scalping gelé jusqu'à Bitvavo USDC).
2. **Ce qui marche le mieux dans la littérature est un portefeuille, pas un signal sur une paire** : rotation sur le top 20 liquide, sortie en cash pendant les bears. Une hypothèse de ce type est la famille F3, repli de F1 (décision du 13/10 (18:35)) : fiche pré-rédigée en 08 §A.6, gelée seulement si F1 ne donne rien. Le seuil de G0 étant maintenu à 300 trades en 1d (décision du 13/10 (21:59)), F3 est rejetée sur papier faute d'effectif, et le repli effectif passe à F4.
3. **Le carry est la seule autre source réelle accessible.** Il sort du périmètre « spot uniquement », réaffirmé par Haythem le 13/10 (18:35) (« pas de perpetual, que du spot » : pas de compte X-Perps, même sans trader de dérivés). Il demanderait une jambe dérivés, un compte X-Perps et une gestion du risque de liquidation ; il reste hors périmètre, sauf nouvelle décision de Haythem.
4. **Les attentes restent celles de 04** : à 300 €, le produit est un système validé, pas un revenu ; le verdict 12 mois se juge avant impôt contre le cash (Livret A net d'impôt, décision du 13/10 (18:35)) ; le buy-and-hold à exposition égale ne sert qu'à l'étiquette « alpha » ou « bêta filtré » (04 §1.1).

## 5. Vérifications effectuées (2026-10-13)

- BIS WP 1087 (pages résumé), page Concretum de *Catching Crypto Trends* (les chiffres de drawdown et de frais ne sont pas sur la page, à lire dans le PDF SSRN), résumé RePEc de *Day Trading for a Living?*, article ForkLog sur les grilles VIP OKX (juillet 2025, grille globale, **à valider** pour l'EEE).
- Non vérifié : performances publiées par les plateformes de bots elles-mêmes (non auditées, non comparables).
