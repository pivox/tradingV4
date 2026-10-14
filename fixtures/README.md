# Fixtures

Tranche XBTUSD réelle du dump Kraken OHLCVT, en trois résolutions. Aucune donnée de marché n'est
inventée ni générée ici.

## Provenance

Fichiers extraits par le propriétaire le 14/10/2026 de `Kraken_OHLCVT_Full.zip` (5 morceaux
réassemblés depuis <https://assets.kraken.com/marketing/institutions/>).

- **SHA-256 du zip** : `bc273e9027470b07d790a7fa4df9bb43f0dfc527c20c6ecae48402e123e7b1c7`, conforme
  à la page support Kraken 360047124832.
- **`MANIFEST.json`** : product `ohlcvt`, schema_version 1, release `r1`, `has_header` false,
  couverture 2013-09-11 → 2026-09-30, 1 760 paires, généré le 2026-10-01.
- **Fichiers du zip** : `XBTUSD_1.csv`, `XBTUSD_5.csv`, `XBTUSD_60.csv`.
- **Extraction** :

```sh
d=1577836800
awk -F, -v d=$d '$1>=d' XBTUSD_1.csv | head -n 10000 > kraken_xbtusd_1m.csv
f=$(tail -n1 kraken_xbtusd_1m.csv | cut -d, -f1)
awk -F, -v d=$d -v f=$f '$1>=d && $1<=f' XBTUSD_5.csv > kraken_xbtusd_5m.csv
awk -F, -v d=$d -v f=$f '$1>=d && $1<=f' XBTUSD_60.csv > kraken_xbtusd_1h.csv
```

## Fichiers

Format : CSV ASCII, fins de ligne LF, sans en-tête, `timestamp,open,high,low,close,volume,trades`,
horodatages en secondes Unix UTC.

| Fichier | Source dans le zip | Lignes | Première bougie (UTC) | Dernière bougie (UTC) | Sert à |
|---|---|---|---|---|---|
| `kraken_xbtusd_1m.csv` | `XBTUSD_1.csv` | 10 000 | 2020-01-01 00:00 (1577836800) | 2020-01-08 21:29 (1578518940) | Lecteur (S2), backtest minimal (S3), backtest de fumée (S8) |
| `kraken_xbtusd_5m.csv` | `XBTUSD_5.csv` | 2 271 | 2020-01-01 00:00 (1577836800) | 2020-01-08 21:25 (1578518700) | Oracle de l'agrégation 1m → 5m (S5, docs/03 §7.5) |
| `kraken_xbtusd_1h.csv` | `XBTUSD_60.csv` | 190 | 2020-01-01 00:00 (1577836800) | 2020-01-08 21:00 (1578517200) | Oracle de l'agrégation 1m → 1h (S5) |

| Fichier | SHA-256 | blake3 (épinglé par `tv4/tests/fixture_kraken.rs`) |
|---|---|---|
| `kraken_xbtusd_1m.csv` | `262547d4d07f2f9cc99ada2a49abfc96ddac9e79c09f42a8d7dae013c121005b` | `3ed72356056ed8df57d6cc8ed0992ad4cbe50940ce08fe52db476842dbf56a4e` |
| `kraken_xbtusd_5m.csv` | `3ca8d963c00b9f8e2eedf6f3142cb83fea7d70f6bd605d2accbd8b3c48c10f82` | `5c0dd275b307ad1f113af17d9a91379a4e8c51a8dea36aac65137e03c40ebb44` |
| `kraken_xbtusd_1h.csv` | `401d9e9faff8e0fa371ee61ff6b9b41bb72057fa1f3806f418333e60cdcc1243` | `8cfbe46812271c5fb51855e89de8809262c32980ba41028ce98c2b0f4e0b5249` |

## Particularités

- **Minutes absentes** : le 1m couvre 11 370 minutes (de 00:00 le 01/01 à 21:29 le 08/01) pour
  10 000 lignes, soit 1 370 minutes absentes. Kraken n'écrit pas les minutes sans transaction ; le
  lecteur et l'agrégation doivent tolérer ces trous.
- **Dernière bougie 1h partielle** : la bougie 21:00 du 1h couvre des minutes après la fin du 1m
  (21:29). Elle est à exclure de l'oracle d'agrégation de S5.

## Règle

Ces fichiers ne se modifient jamais. Un changement = de nouveaux fichiers et de nouvelles empreintes
(SHA-256 et blake3). `.gitattributes` interdit toute conversion de fins de ligne sur ce dossier, et
`tv4/tests/fixture_kraken.rs` échoue si un octet change.
