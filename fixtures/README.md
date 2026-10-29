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

## Fichiers golden Parquet (S4)

| Fichier | Contenu |
|---|---|
| `golden/kraken_BTC-USD_1m_2024-01.parquet` | Fichier Parquet du store : kraken, BTC-USD, 1m, mois 2024-01, 3 lignes |
| `golden/kraken_BTC-USD_1m_2024-01._manifest.json` | Son manifeste (`closed` vrai, `source` `golden`, `ingested_at` `2026-10-14T00:00:00Z`) |

Contenu : les trois bougies du vecteur de `docs/dataset-hash.md` §7. Ce sont des **données de test, pas
des données de marché** : la provenance de ces prix est le document de spécification. Les fichiers sont
à plat sous `fixtures/golden/` (le motif `data/` de `.gitignore` ignorerait un dossier `data`), et les
tests les copient dans l'arborescence d'une racine temporaire.

Production, une seule fois, par `tv4` lui-même (`write_partition` avec l'instant d'écriture
`1791936000000`, soit 2026-10-14T00:00:00Z, pour un manifeste déterministe) :

```sh
TV4_REGENERER_GOLDEN=1 cargo test --locked --test store -- --ignored produire_les_fichiers_golden --nocapture
```

Empreintes attendues :

| Grandeur | Valeur |
|---|---|
| `content_hash` (fenêtre du mois, `skip`) | `blake3:3da10e85c1fdd2673a5cc9a0ea7dbd2bf5c171e3f11c23e04dff603392bdd8ab` |
| `dataset_hash` sur `[1704067200000, 1704067380000)`, `skip` | `blake3:a15f886ff621e7e10b7d4769793d818b23686441a2ce3d4dd6a4918b740cae8e` |
| blake3 des octets du Parquet | `fdb3082449843de1c929599cde12d128ea4502c59469212e6fed01bbf3abb87b` |
| SHA-256 des octets du Parquet | `f463e7f3174b7f1836243592f07fed45339d0ef1232fbaf9ff2bfb0be251e32d` |
| SHA-256 du manifeste | `f4693555967bd9e4f5243ca3f61e2047d28f4c1d70ccc5d7c9c44efa8370ab63` |

`tv4/tests/store.rs` épingle ces valeurs (`golden_fichiers_epingles` pour les octets,
`golden_parquet_content_hash_et_dataset_hash` pour les hashes logiques). Une montée d'`arrow` ou de
`parquet` qui changerait la lecture du fichier ou le hash logique est donc bloquée en CI ; les octets
du Parquet, eux, peuvent changer avec une montée de version (le footer contient `created_by`), auquel
cas on régénère et on met à jour les empreintes. Le `content_hash` du mois 2020-01 de
`kraken_xbtusd_1m.csv` une fois ingéré (`blake3:d94f2eb46e2c4b6738b7ef4ed2441f3928db379e5b29f48f95532ba18c56f353`)
est aussi épinglé dans `tv4/tests/store.rs`.

## Zips synthétiques des tests (S5)

Les tests de `tv4 ingest kraken` (`tv4/tests/ingest_kraken.rs`) construisent leurs zips à la volée avec
la crate `zip` en écriture : aucun zip n'est commité. Un zip de test reprend la structure du dump réel
(entrées à la racine nommées `<PAIRE>_<minutes>.csv`, sans en-tête, plus un `MANIFEST.json`), avec les
lignes réelles de ces trois fichiers pour XBTUSD. Les contenus inventés sont annoncés comme tels dans
le code : le `MANIFEST.json` (les noms de clés du manifeste réel ne sont pas connus, le lecteur est
tolérant), l'entrée piège `AIXBTUSD_1.csv` (la recherche se fait par nom exact, jamais par suffixe) et
l'échantillon `ETHUSD_1.csv`. Le dump complet n'est pas joignable depuis le conteneur de développement :
il n'est lu que sur le poste du propriétaire.

## Commande à lancer sur le poste du propriétaire (S5)

À lancer après la fusion de la PR de S5, dans un terminal zsh sur le Mac, ligne par ligne. La séquence
ne contient aucun commentaire en ligne (zsh interactif ne les accepte pas sans `setopt
interactivecomments`) et aucun tube après `tv4`, pour que le code de sortie affiché soit celui de `tv4`.
La racine dédiée `~/data/tv4` est recommandée : les fichiers clos y sont immuables, et une racine de test
mélangerait le dump avec la fixture ingérée par `tv4 ingest csv`. Le chemin du zip est à remplacer s'il
diffère.

```zsh
cd ~/workspace/perso/tradingV4
git switch main
git pull --ff-only
cargo build --release --locked
mkdir -p ~/data/tv4
./target/release/tv4 ingest kraken --root ~/data/tv4 --zip ~/data/kraken/Kraken_OHLCVT_Full.zip --sha256 bc273e9027470b07d790a7fa4df9bb43f0dfc527c20c6ecae48402e123e7b1c7 --pair XBTUSD --pair ETHUSD > ~/data/tv4/ingest-kraken.log 2>&1; echo "code de sortie : $?"
cat ~/data/tv4/ingest-kraken.log
ls -la ~/data/tv4/reports/ingest/kraken
```

Le SHA-256 du zip (`bc273e90...b1c7`, celui de la section Provenance) figure dans cette commande et dans
la documentation, jamais dans le code : le binaire ne connaît aucun hash en dur.

À renvoyer au pilote : le code de sortie affiché, le fichier `~/data/tv4/ingest-kraken.log` et les trois
fichiers `~/data/tv4/reports/ingest/kraken/_summary.json`, `BTC-USD.json` et `ETH-USD.json`. Ils ne
contiennent aucun secret (chemins locaux, compteurs, hash, manifeste du zip). Le champ `raw` du
manifeste dans `_summary.json` donne les vrais noms de clés : si `coverage_end` y est `null`, le pilote
ajoutera la clé réelle à la liste des candidats du lecteur (`tv4/src/marketdata/kraken_zip.rs`), sans
autre changement. Durée attendue : quelques dizaines de secondes (SHA-256 du zip entier, puis deux
paires) ; pic de mémoire de l'ordre de 1 Go pendant XBTUSD.

## Commandes du propriétaire (S11)

À lancer après la fusion de la PR de S11, dans un terminal zsh sur le Mac, avec le même zip et la même racine
`~/data/tv4` que pour S5. Les séquences sont dans [`docs/specs/s11.md`](../docs/specs/s11.md) §15, qui fait foi
(mêmes règles : aucun commentaire en ligne, aucun tube après `tv4`, aucune commande ne lit un fichier de clé) :

- §15.2 : test de survivance et univers trimestriel (`tv4 universe survivorship`, `tv4 universe build --at`), avec les
  comptes 5 / 8 / 9 de 04 §2.2 à retrouver ;
- §15.3 : ingestion de l'union des paires (`tv4 ingest kraken --pairs-from-universe`) puis `tv4 universe verify` ;
- §15.4 : les quatre datasets de recherche (`tv4 dataset build --block`) et leurs `dataset_hash` ;
- §15.5 : run de démonstration sur DEV, deux fois, comparé par `tv4 verify` ;
- §15.6 : liste OKX en USDC, une fois la clé en lecture seule créée (S9 §19).

Les fichiers à renvoyer y sont nommés, et aucun ne contient de secret. Cette fixture n'est pas lue par ces commandes ;
elle sert aux fumées de CI de S11 (`.github/workflows/ci.yml`).

## Règle

Ces fichiers ne se modifient jamais. Un changement = de nouveaux fichiers et de nouvelles empreintes
(SHA-256 et blake3). `.gitattributes` interdit toute conversion de fins de ligne sur ce dossier, et
`tv4/tests/fixture_kraken.rs` échoue si un octet change. Les fichiers golden Parquet ne se régénèrent
que par la commande ci-dessus, avec mise à jour des empreintes du tableau et de `tv4/tests/store.rs`.
