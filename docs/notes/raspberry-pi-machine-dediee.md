# Faire tourner tradingV4 sur un Raspberry Pi : étude

Date : 2026-10-13. Thread « Raspberry Pi ». Étude seulement, aucun code. Destinataire : propriétaire de 03 et 08 (thread Plan), pour les deux corrections proposées en fin de note.

## 1. Ce que la machine maison doit faire (rappel, 03 §2 et 08 §C.2)

Paper (G5) puis live (G6, G7) : binaire `tv4` (`execution` sous `systemd`, `WatchdogSec=60`), PostgreSQL, Symfony (IHM), Mercure au jalon 3, le tout en `docker compose`, joignable par WireGuard seul. Univers live : 5 paires, décisions 1d et 4h, flux WebSocket 1m, réconciliation < 1 requête/s. Pas de campagne de recherche ni de `cargo build` dessus.

La charge est minuscule : quelques messages WebSocket par seconde, une décision toutes les 4 h, une base de quelques centaines de Mo. N'importe quel Pi 5 la tient. La question n'est pas la puissance, c'est le prix, la fiabilité et le coût d'une deuxième architecture CPU.

## 2. Faisabilité technique : oui, sous conditions

| Point | Constat | Condition |
|---|---|---|
| CPU | Pi 5 : 4 × Cortex-A76 2,4 GHz, très au-dessus du besoin | aucune |
| RAM | estimation : OS + Docker ≈ 0,5 Go, PostgreSQL ≈ 0,3-0,5 Go, PHP/Symfony ≈ 0,3 Go, `tv4` < 0,1 Go, soit < 2 Go (**estimé, à mesurer pendant le paper**) | 4 Go suffit ; 8 Go seulement pour de la marge |
| Stockage | carte SD exclue (03 §2 : jamais PostgreSQL sur SD, usure et corruption) | démarrage et données sur SSD NVMe (M.2 HAT+ ou équivalent) |
| Alimentation | sous-alimenté, le Pi 5 bride l'USB et le PCIe | alimentation officielle 27 W |
| Chaleur | throttling sans refroidissement actif | Active Cooler ou boîtier ventilé |
| Horloge | le Pi 5 a une RTC mais **sans pile fournie** ; sans elle, l'heure est fausse au boot tant que NTP n'a pas répondu | pile RTC officielle + `systemd-time-wait-sync` (déjà prévu en 08 §C.2) |
| Rust | `aarch64-unknown-linux-gnu` est une cible tier 1 | build croisé sur le poste de dev (`cross` ou `cargo-zigbuild`), jamais `cargo build` sur le Pi |
| Docker | images officielles `postgres` et `php` multi-arch arm64 | aucune |
| Déterminisme | 03 §7.4 : le chemin de calcul n'utilise que + − × ÷ IEEE 754 et `sqrt` exacts, Rust ne contracte pas en FMA ; un binaire aarch64 devrait donc donner les mêmes signaux que le x86-64 | **à valider** : faire tourner le golden test de CI aussi en aarch64 (runner arm64 ou QEMU) avant G5 ; `ln`/`exp` restent confinés dans `report` |

## 3. Prix au 13/10/2026 : l'estimation de 08 §C.2 est fausse

La pénurie de DRAM a fait monter les prix du Pi trois fois en 2026 (décembre 2025, février, avril, puis le 2 Go en octobre).

| Élément | Prix | Source |
|---|---|---|
| Pi 5 4 Go | 110 $ ; 136,80 € TTC chez Kubii ce jour | Phoronix 01/10/2026, page Kubii |
| Pi 5 8 Go | 180 $ ; ≈ 204 € TTC (Kubii, juillet 2026) | Phoronix, Hardware & Co 06/07/2026 |
| Kit SSD officiel (M.2 HAT+ + SSD 256 Go) | ≈ 55 € ; adaptateur seul ≈ 13 €, SSD 256 Go ≈ 35 € | raspberry.tips, prix du 13/07/2026 |
| Alimentation 27 W, Active Cooler, pile RTC, boîtier | ≈ 30-40 € (**à valider**) | non vérifié |
| **Pi 5 4 Go complet** | **≈ 220-235 €** | somme |
| **Pi 5 8 Go complet** | **≈ 290-300 €** | somme |

08 §C.2 donne « ≈ 80 à 120 € une fois (à valider) » : c'était le prix de 2024. Il faut compter plus du double.

En face, un mini-PC x86 de bureau reconditionné (HP EliteDesk 800 G3 Mini, i5-6400T, 8 Go, SSD 240 Go) se trouve ≈ 160 € et un Lenovo M73 Tiny 16 Go ≈ 179 € (minipc-review.com, 05/2026). Il consomme plus au repos (≈ 8-12 W contre ≈ 4-5 W pour le Pi avec NVMe, **ordres de grandeur à valider**), soit ≈ 10 €/an d'électricité en plus à ≈ 0,20 €/kWh (**à valider**).

## 4. Le coût matériel pèse sur le verdict

À 300 € de capital, 230 € de matériel amorti sur 3 ans font ≈ 75 €/an, soit 25 % du capital par an. 08 §C.2 traite déjà le VPS (60 à 120 €/an) comme un coût de procédé rapporté à côté du verdict (04 §3.3) ; un achat de matériel dédié relève de la même logique. D'où l'ordre de préférence :

1. **Une machine déjà possédée et inutilisée** (Pi 5 ≥ 4 Go, vieux portable ou mini-PC x86, pas le poste de dev) : coût ≈ 0 à 55 €. Un vieux portable x86 a un avantage net : sa batterie sert d'onduleur.
2. **Un mini-PC x86 reconditionné** (≈ 150-180 €) : moins cher qu'un Pi 5 complet, pas de deuxième architecture, pas de double CI.
3. **Un Pi 5 neuf** (≈ 220-235 € en 4 Go) : plus cher, plus sobre, plus silencieux ; il impose le build aarch64 et sa validation.

## 5. Ce qui reste exposé quelle que soit la machine

- **Coupure de courant** : la box tombe aussi, donc un onduleur sur le Pi seul ne sert à rien ; il faudrait couvrir box + machine. Les stops vivent sur OKX (algo, 08 §C.2), une coupure n'ouvre jamais de position et retarde au pire une sortie de signal. Pas d'onduleur au départ ; mesurer les coupures pendant le paper (même critère que le VPS : > 15 min plus d'une fois par mois).
- **Arrêt brutal** : PostgreSQL survit à un arrêt sale sur SSD avec `fsync` actif ; la reprise passe de toute façon par la réconciliation (08 §C.2).
- **Usure** : SSD NVMe sans DRAM conseillé sur le Pi (le câble ne fournit que ≈ 5 W) ; sauvegarde quotidienne chiffrée déjà prévue.

## 6. Recommandation

Le Pi 5 convient techniquement, avec SSD NVMe, alimentation 27 W, refroidissement actif et pile RTC. Mais acheté neuf en octobre 2026, il coûte plus cher qu'un mini-PC x86 reconditionné et ajoute une architecture à valider. Recommandé : réutiliser une machine existante, sinon un mini-PC x86 reconditionné ; le Pi 5 seulement si Haythem en a déjà un (4 Go ou plus). La décision peut attendre G5 (paper), donc bien après le jalon 1 : les prix auront encore bougé.

## 7. Corrections proposées à 03 et 08 (pour le thread Plan)

1. 08 §C.2, colonne Coût : remplacer « matériel existant ou ≈ 80 à 120 € une fois (à valider) » par « matériel existant (≈ 0 à 55 €), mini-PC x86 reconditionné ≈ 150-180 €, Pi 5 4 Go complet ≈ 220-235 € (prix 10/2026, pénurie DRAM) ; un achat dédié est rapporté comme coût de procédé à côté du verdict (04 §3.3) ».
2. 03 §2 « Machine » et 08 §C.2 : préciser les conditions du Pi 5 (alimentation 27 W, refroidissement actif, boot NVMe, pile RTC) et ajouter « golden test CI en aarch64 avant G5 » comme condition du choix Pi.

## Sources

- Phoronix, « Raspberry Pi 5 2GB Price Increases To $77.50 USD », 01/10/2026 : https://www.phoronix.com/news/Raspberry-Pi-5-2GB-Price-77.50
- Raspberry Pi, hausse d'avril 2026 : https://www.raspberrypi.com/news/a-new-3gb-raspberry-pi-4-for-83-75-and-more-memory-driven-price-increases/
- Tom's Hardware, hausse d'octobre 2026 : https://www.tomshardware.com/raspberry-pi/component-shortages-drive-raspberry-pi-prices-up-by-up-to-23-percent-escalating-lpddr4-lpddr5-costs-trigger-the-third-price-hike-of-the-year
- Jeff Geerling, « DRAM pricing is killing the hobbyist SBC market », 01/04/2026 : https://www.jeffgeerling.com/blog/2026/dram-pricing-is-killing-the-hobbyist-sbc-market/
- Kubii, Raspberry Pi 5 : https://www.kubii.com/fr/cartes-nano-ordinateurs/4106-1831-raspberry-pi-5-3272496315938.html
- Hardware & Co, prix Kubii juillet 2026 : https://hardwareand.co/actualites/breves/cest-lete-il-faut-chaud-mais-les-prix-des-raspberry-pi-eux-font-froid-dans-le-dos
- raspberry.tips, guide NVMe Pi 5 (prix du 13/07/2026) : https://raspberry.tips/en/raspberrypi-tutorials/nvme-ssd-raspberry-pi-5-buying-guide
- minipc-review.com, mini-PC reconditionnés 2026 : https://minipc-review.com/fr/mini-pc-reconditionns-petit-prix-les-meilleurs-de-2026
