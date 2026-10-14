# Fixtures

Ce dossier ne contient, pour l'instant, aucune donnée.

## Fixtures attendues : tranche XBTUSD du dump Kraken

Trois fichiers réels, à fournir par le propriétaire (le réseau vers Kraken est bloqué dans
l'environnement de développement), extraits tels quels d'un `Kraken_OHLCVT_*.zip`. Aucune donnée de
marché ne doit être inventée ni générée ici.

| Fichier | Source dans le zip | Contenu | Sert à |
|---|---|---|---|
| `kraken_xbtusd_1m.csv` | `XBTUSD_1.csv` | 10 000 bougies 1m à partir du 01/01/2020 00:00 UTC (bloc DEV) | Lecteur (S2), backtest minimal (S3), backtest de fumée en CI |
| `kraken_xbtusd_5m.csv` | `XBTUSD_5.csv` | Même période | Oracle de l'agrégation 1m → 5m (S5, docs/03 §7.5) |
| `kraken_xbtusd_1h.csv` | `XBTUSD_60.csv` | Même période | Oracle de l'agrégation 1m → 1h (S5) |

- **Format** : les 7 colonnes `timestamp,open,high,low,close,volume,trades`, sans en-tête, horodatages
  en secondes Unix UTC.
- **À noter ci-dessous à l'arrivée des fichiers** : nom du zip, SHA-256 publié par Kraken, plage
  d'horodatages (première et dernière bougie) et SHA-256 de chaque fichier extrait.

Le test d'intégration du lecteur (`tv4/src/marketdata/kraken.rs`) sur `kraken_xbtusd_1m.csv` sera
ajouté à son arrivée. D'ici là, les tests du lecteur utilisent de petits échantillons de format écrits
à la main dans le code de test (valeurs inventées, prix ronds autour de 100), explicitement présentés
comme tels.
