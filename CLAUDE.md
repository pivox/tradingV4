# tradingV4

Plateforme de trading crypto **spot** sur OKX : moteur de backtest, de paper trading et d'exécution en Rust, control plane Symfony. Objectif fixé par le propriétaire : un système qui trade réellement et dégage un PnL net positif, démontré par un protocole de recherche avant tout euro engagé.

## État du dépôt

- Conception terminée le 2026-10-13 : documents 01 à 08 dans `docs/`. Les trois points de `docs/03-architecture.md` §13 sont tranchés (décisions du 13/10, 21:47). Reste à trancher par le propriétaire : le seuil d'effectif de F3 (`docs/08-strategies-risque-exploitation.md` §A.6), avant son gel.
- **Aucun code applicatif avant le feu vert explicite du propriétaire.** Le jalon 1 est découpé en issues S1 à S8 (`docs/03-architecture.md` §11).

## Documents, à lire avant toute modification

| Fichier | Contenu |
|---|---|
| `docs/01-cadrage.md` | Objectif, ce qu'on reprend de tradingV3 (les idées, pas le code), périmètre du jalon 1 |
| `docs/02-matrice-exchanges.md` | Choix de l'exchange : Binance exclu pour un résident français, OKX spot retenu |
| `docs/03-architecture.md` | Architecture du moteur Rust, modules, performance, jalon 1 détaillé (S1 à S8) |
| `docs/04-protocole-recherche.md` | Protocole de recherche d'edge : blocs DEV / VALIDATION / HOLDOUT, portes G0 à G7, paper et live |
| `docs/05-ihm.md` | IHM du control plane Symfony |
| `docs/06-lacunes-bots-marche.md` | Ce qui manque aux bots du marché : journal fiscal, contraintes d'exchange, sécurité de la clé |
| `docs/07-sources-de-rentabilite.md` | D'où vient le PnL des bots, et ce qui survit à nos frais |
| `docs/08-strategies-risque-exploitation.md` | Familles de stratégies, risque et capital à 300 €, mode opératoire, décisions du 13/10 |

## Décisions du propriétaire (ne se rediscutent pas sans lui)

- Crypto **spot uniquement**, aucun compte ni produit dérivé ; exécution OKX spot, frais 0,20 % maker / 0,35 % taker.
- Jalon 1 : Parquet seul (PostgreSQL au jalon 2), stratégies en Rust pur sans scripting, périmètre complet en 8-12 semaines à mi-temps (`docs/03-architecture.md` §13).
- Moteur en Rust ; control plane Symfony au jalon 2, précédé au jalon 1 d'un squelette Symfony qui rend le rapport HTML statique (`docs/03-architecture.md` §11, S8).
- Données : dumps OHLCVT Kraken et `history-candles` OKX ; paper trading sur le WebSocket public.
- Capital de départ 300 € au maximum ; pilote en échelle 20 % → 50 % → 100 % ; verdict sur 12 mois à 100 %, avant impôt, contre le Livret A.
- Recherche limitée au 1d et au 4h sur OKX (1h en maker seulement, hors cycle 1) ; F1 (cassure Donchian 55 + filtre BTC > SMA200) en tête, F3 en repli.
- Un seul utilisateur, aucune notification sortante ; IHM accessible par VPN seul ; machine dédiée à la maison.

## Règles de travail

- Documentation, commits et échanges en français.
- Jamais de secret dans le dépôt (clés API, fichiers `.env`) ; la clé OKX n'a jamais le droit de retrait.
- Toute hypothèse de recherche passe par une fiche pré-enregistrée et commitée avant tout run sur DEV (`docs/04-protocole-recherche.md` §4.3), puis par les portes G0 à G7.
- Un résultat de backtest n'est valide qu'avec son `config_hash`, son `dataset_hash` et le commit du moteur.
- Pas de score composite ni d'empilement multi-timeframe à score : abandonnés après tradingV3.
