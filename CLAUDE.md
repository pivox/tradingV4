# tradingV4

Plateforme de trading crypto **spot** sur OKX : moteur de backtest, de paper trading et d'exécution en Rust, control plane Symfony. Objectif fixé par le propriétaire : un système qui trade réellement et dégage un PnL net positif, démontré par un protocole de recherche avant tout euro engagé.

## État du dépôt

- Conception terminée le 2026-10-13 : documents 01 à 08 dans `docs/`. Les trois points de `docs/03-architecture.md` §13 sont tranchés (décisions du 13/10, 21:47), ainsi que le seuil d'effectif de G0, maintenu à 300 trades en 1d (13/10, 21:59, `docs/08-strategies-risque-exploitation.md` §A.6). Plus aucun point de conception n'est en suspens ; restent à lire sur le compte OKX Europe la grille de frais et la liste des paires USDC autorisées avec leurs minimums (`GET /api/v5/account/instruments`), et à remesurer l'univers live sur les médianes 1m des carnets USDC.
- **Aucun code applicatif avant le feu vert explicite du propriétaire.** Le jalon 1 est découpé en issues S1 à S8 (`docs/03-architecture.md` §11).

## Documents, à lire avant toute modification

| Fichier | Contenu |
|---|---|
| `docs/01-cadrage.md` | Objectif, ce qu'on reprend de tradingV3 (les idées, pas le code), périmètre du jalon 1 |
| `docs/02-matrice-exchanges.md` | Choix de l'exchange : Binance exclu pour un résident français, Bybit EU étudié le 13/10 et non retenu, OKX spot retenu |
| `docs/03-architecture.md` | Architecture du moteur Rust, modules, performance, jalon 1 détaillé (S1 à S8) |
| `docs/04-protocole-recherche.md` | Protocole de recherche d'edge : blocs DEV / VALIDATION / HOLDOUT, portes G0 à G7, paper et live |
| `docs/05-ihm.md` | IHM du control plane Symfony |
| `docs/06-lacunes-bots-marche.md` | Ce qui manque aux bots du marché : journal fiscal, contraintes d'exchange, sécurité de la clé |
| `docs/07-sources-de-rentabilite.md` | D'où vient le PnL des bots, et ce qui survit à nos frais |
| `docs/08-strategies-risque-exploitation.md` | Familles de stratégies, risque et capital à 300 €, mode opératoire, décisions du 13/10 |

## Décisions du propriétaire (ne se rediscutent pas sans lui)

- Crypto **spot uniquement**, aucun compte ni produit dérivé ; exécution OKX Europe spot en paires USDC, décision du 13/10 (22:46), l'USDT n'étant pas échangeable pour un résident de l'EEE ; frais 0,20 % maker / 0,35 % taker, grille du compte à confirmer (page Frais ou `GET /api/v5/account/trade-fee`) avant tout recalcul, un avis OKX du 11/09/2026 annonçant 0,10 % / 0,20 % pour le compte spot seul à partir du 25/09/2026.
- Jalon 1 : Parquet seul (PostgreSQL au jalon 2), stratégies en Rust pur sans scripting, périmètre complet en 8-12 semaines à mi-temps (`docs/03-architecture.md` §13).
- Moteur en Rust ; control plane Symfony au jalon 2, précédé au jalon 1 d'un squelette Symfony qui rend le rapport HTML statique (`docs/03-architecture.md` §11, S8).
- Données : dumps OHLCVT Kraken et `history-candles` OKX ; paper trading sur le WebSocket public de l'EEE (`wseea.okx.com`).
- Capital de départ 300 € au maximum ; pilote en échelle 20 % → 50 % → 100 % ; verdict sur 12 mois à 100 %, avant impôt, contre le Livret A.
- Recherche limitée au 1d et au 4h sur OKX (1h en maker seulement, hors cycle 1) ; F1 (cassure Donchian 55 + filtre BTC > SMA200) en tête ; F3 documentée mais rejetée par G0 (seuil de 300 trades en 1d maintenu), repli effectif F4.
- Un seul utilisateur, aucune notification sortante ; IHM accessible par VPN seul ; machine dédiée à la maison.

## Règles de travail

- Documentation, commits et échanges en français.
- Jamais de secret dans le dépôt (clés API, fichiers `.env`) ; la clé OKX n'a jamais le droit de retrait, et le moteur passe `tdMode = cash` sans jamais activer l'emprunt (OKX Europe propose la marge spot jusqu'à 10×).
- Toute hypothèse de recherche passe par une fiche pré-enregistrée et commitée avant tout run sur DEV (`docs/04-protocole-recherche.md` §4.3), puis par les portes G0 à G7.
- Un résultat de backtest n'est valide qu'avec son `config_hash`, son `dataset_hash` et le commit du moteur.
- Pas de score composite ni d'empilement multi-timeframe à score : abandonnés après tradingV3.
