# Encodage canonique `dsh-v1` des données de marché

Spécification du format de hachage `dsh-v1`, qui produit le `content_hash` d'un fichier de bougies et le `dataset_hash` d'un dataset figé (`docs/03-architecture.md` §5.3). Ce document fait foi : l'implémentation Rust de S4 (`tv4/src/store/dataset.rs`) le suit, et le control plane doit pouvoir recalculer le même hash en PHP au jalon 2 à partir des mêmes bougies, sans lire le code Rust.

Valeur de `hash_format` dans les manifestes : `"dsh-v1"`.

## 1. Principe

Le hash porte sur le **contenu logique** des bougies, sérialisé dans un flux d'octets possédé par le projet. Il ne dépend ni des octets du fichier Parquet (le footer contient `created_by` et la version de `parquet-rs`), ni de la compression, ni du découpage en row groups, ni de la taille des `RecordBatch` Arrow, ni des buffers Arrow et de leur bitmap de validité, ni d'un schéma rendu par `format!("{schema:?}")` ou des métadonnées `ARROW:schema`. Deux fichiers Parquet différents qui contiennent les mêmes bougies ont le même `content_hash`.

Le flux est fait d'un ou plusieurs **segments**, un par série (exchange, symbole, timeframe). Chaque segment est un en-tête texte suivi des lignes binaires de la série. Le flux entier passe dans **un seul** `blake3::Hasher`.

## 2. En-tête d'un segment

### 2.1 Format

```
tradingv4-dataset-v1|<exchange>|<symbol>|<tf>|<window>|<gaps_policy>|<row_count>\n
```

- Le préfixe `tradingv4-dataset-v1` est écrit tel quel.
- Les sept champs (préfixe compris) sont séparés par le caractère `|` (0x7C), sans espace avant ni après.
- L'en-tête se termine par un saut de ligne `\n` (0x0A), un seul, sans `\r`. Il est obligatoire : il sépare sans ambiguïté le dernier chiffre de `row_count` du premier octet binaire de la première ligne, y compris quand plusieurs segments se suivent. C'est une précision du littéral de 03 §5.3, qui ne mentionnait pas de terminateur.
- L'en-tête est encodé en UTF-8 sans BOM. Les jeux de caractères autorisés ci-dessous sont tous ASCII, donc l'UTF-8 se réduit à un octet par caractère.
- L'en-tête est écrit à la main dans le code (`format!` sur les champs ci-dessous), jamais dérivé d'un schéma Arrow ou d'une sérialisation `Debug`.

### 2.2 Champs

| Champ | Contenu | Jeu de caractères | Exemple |
|---|---|---|---|
| `exchange` | identifiant canonique de l'exchange, en minuscules | `[a-z0-9_]+` | `kraken`, `okx` |
| `symbol` | symbole canonique `BASE-QUOTE`, en majuscules (jamais le symbole natif de l'exchange : `XBTUSD` devient `BTC-USD`) | `[A-Z0-9]+-[A-Z0-9]+` | `BTC-USD`, `BTC-USDC` |
| `tf` | rendu canonique de `Timeframe` | une des valeurs `1m`, `5m`, `15m`, `30m`, `1h`, `4h`, `12h`, `1d` (`M1` à `D1` de 03 §6) | `1m` |
| `window` | fenêtre semi-ouverte `[start, end)` en millisecondes epoch UTC, rendue `<start_ms>,<end_ms>` | décimal ASCII, virgule (0x2C) | `1704067200000,1767225600000` |
| `gaps_policy` | politique de trous du dataset (03 §5.1) | `skip` ou `ffill` | `skip` |
| `row_count` | nombre de lignes du segment, **après** filtrage par la fenêtre et dédoublonnage (§3) | décimal ASCII | `1051200` |

Rendu des entiers (`start_ms`, `end_ms`, `row_count`) : base 10, chiffres ASCII, sans zéro de tête (`0` pour zéro), sans signe `+`, sans séparateur de milliers, sans espace. Un `-` de tête est permis pour un entier négatif (horodatage avant 1970), ce qui n'arrive pas en pratique. Contraintes : `start_ms < end_ms`.

Les fenêtres des manifestes (`"window": ["2024-01-01T00:00:00Z", "2026-01-01T00:00:00Z"]`) sont converties en millisecondes epoch UTC avant d'entrer dans l'en-tête. L'en-tête ne contient jamais de date au format texte, ce qui évite toute dépendance au formatage des dates (et à `chrono` dans `model`).

Un champ qui sort de son jeu de caractères est une **erreur** : le calcul du hash échoue, il ne nettoie ni ne remplace rien. Cela garantit qu'aucun champ ne contient `|` ni `\n`.

## 3. Lignes d'un segment

### 3.1 Sélection, tri et dédoublonnage

1. **Sélection** : seules les bougies de la série dont `start_ms <= open_time_ms < end_ms` sont hachées. Les bougies absentes (trous) ne sont **jamais synthétisées** pour le hash, y compris avec `gaps_policy = ffill` : le remplissage est appliqué au chargement, de façon déterministe à partir des lignes stockées, et sa présence dans l'en-tête suffit à distinguer les deux hashes.
2. **Tri** : par `open_time_ms` strictement croissant (comparaison d'entiers signés 64 bits).
3. **Dédoublonnage** : deux lignes de même `open_time_ms` sont des doublons.
   - Si leurs 53 octets encodés (§3.2, après canonicalisation §3.3) sont identiques, une seule est gardée.
   - S'ils diffèrent, le calcul du hash **échoue** avec une erreur qui nomme l'horodatage. Le hash n'arbitre jamais entre deux versions d'une bougie : c'est le rôle de l'ingestion (`continuity.rs`, S5 et S7), qui dédoublonne avant d'écrire le fichier.
4. `row_count` dans l'en-tête est le nombre de lignes restant après ces trois étapes. Il est connu avant d'écrire l'en-tête, puisque les lignes sont triées en mémoire au préalable.

### 3.2 Encodage binaire d'une ligne (53 octets)

Les lignes sont écrites l'une après l'autre, sans séparateur, sans marqueur de début ou de fin, sans compteur intermédiaire.

| Octets | Champ | Encodage |
|---|---|---|
| 0 à 7 | `open_time_ms` | entier signé 64 bits, complément à deux, little-endian |
| 8 à 15 | `open` | bits IEEE 754 binary64 canonicalisés (§3.3), little-endian |
| 16 à 23 | `high` | idem |
| 24 à 31 | `low` | idem |
| 32 à 39 | `close` | idem |
| 40 à 47 | `volume` | idem |
| 48 | présence de `trades` | `0x01` si `trades` est connu, `0x00` s'il est absent |
| 49 à 52 | `trades` | entier non signé 32 bits little-endian ; `0x00000000` quand `trades` est absent |

`trades` absent (`None`, colonne nulle dans le Parquet, cas OKX) et `trades` égal à zéro (`Some(0)`) donnent des octets différents (`00 00000000` contre `01 00000000`) et donc des hashes différents.

### 3.3 Canonicalisation des flottants

Avant d'écrire les bits d'un flottant :

- `-0.0` (bits `0x8000000000000000`) est remplacé par `+0.0` (bits `0x0000000000000000`). Règle : toute valeur égale à zéro au sens IEEE (`x == 0.0`) s'écrit `0x0000000000000000`.
- Tout NaN, quels que soient son signe et sa charge utile, est remplacé par le NaN canonique **`0x7FF8000000000000`** (NaN silencieux positif, charge nulle ; ce sont les bits de `f64::NAN` en Rust). Le langage ne garantit pas la charge d'un NaN, d'où la canonicalisation. En pratique un NaN n'atteint jamais le hash : il est rejeté à l'ingestion (03 §7.4).
- Les autres valeurs, infinis compris, sont écrites telles quelles. Leur rejet éventuel relève de l'ingestion (`ohlc_errors`), pas du hash.

## 4. Hachage et rendu

- Fonction : BLAKE3 standard, sortie de 32 octets (`blake3::Hasher::new()`, ni mode `keyed`, ni `derive_key`, ni sortie étendue), crate `blake3` 1.8.7.
- **Un seul `blake3::Hasher` incrémental** reçoit tout le flux : en-tête du premier segment, ses lignes, en-tête du segment suivant, ses lignes, etc. Aucun marqueur de batch, de row group, de fichier ou de mois n'est inséré. Le découpage des appels à `update` est libre (une ligne, un mois, un `RecordBatch` à la fois) : BLAKE3 donne le même résultat pour le même flux d'octets, quelle que soit la façon de le fournir.
- Rendu : les 32 octets en hexadécimal **minuscule**, 64 caractères, sans séparateur (`Hash::to_hex()`).
- Dans les manifestes JSON, la valeur est préfixée par l'algorithme : `"content_hash": "blake3:<64 hex>"`, `"dataset_hash": "blake3:<64 hex>"`, accompagnée de `"hash_format": "dsh-v1"`. Le préfixe ne fait pas partie du flux haché.

## 5. `content_hash` et `dataset_hash`

### 5.1 `content_hash` d'un fichier Parquet

Un seul segment, avec :

- `exchange`, `symbol`, `tf` : ceux de la partition du fichier ;
- `window` : la période calendaire du fichier en UTC, `[premier instant du mois, premier instant du mois suivant)` pour un fichier mensuel (1m, 5m), `[1er janvier, 1er janvier suivant)` pour un fichier annuel (TF ≥ 1h) ; la fenêtre reste la même pour le mois courant encore mutable, seul `row_count` et les lignes changent ;
- `gaps_policy` : `skip`, puisqu'un fichier stocke les bougies sans synthèse ;
- les lignes : toutes celles du fichier. Une ligne dont l'`open_time_ms` tombe hors de la `window` du fichier est une erreur (le calcul échoue, rien n'est ignoré en silence), de même que les doublons qui diffèrent (§3) ; le filtrage par la fenêtre du §3 ne s'applique donc, pour un fichier, qu'à des lignes toutes dans la fenêtre.

Conséquence voulue : le `content_hash` d'un fichier mensuel est égal au `dataset_hash` d'un dataset mono-série `skip` dont la fenêtre est ce mois.

### 5.2 `dataset_hash` d'un dataset figé

Un segment par couple (symbole, timeframe) du dataset, tous avec l'`exchange`, la `window` et la `gaps_policy` du manifeste du dataset. Chaque segment contient les lignes de cette série, tous fichiers confondus, sélectionnées, triées et dédoublonnées selon §3.

Ordre des segments : tri lexicographique sur le couple `(symbol, tf)`, chaque champ comparé **octet par octet** (`strcmp` en PHP, `Ord` de `str` en Rust), `symbol` d'abord et `tf` seulement à `symbol` égal, et non par durée du timeframe. On ne trie donc pas la chaîne concaténée `symbol|tf` : un symbole préfixe d'un autre passe avant lui, quel que soit le `tf` (`BTC-USD` précède `BTC-USDC`, alors que la concaténation placerait `BTC-USDC|…` avant `BTC-USD|…`, car `C` (0x43) < `|` (0x7C)). Exemple d'ordre : `(BTC-USD, 15m)`, `(BTC-USD, 1d)`, `(BTC-USD, 1h)`, `(BTC-USD, 1m)`, `(BTC-USDC, 1d)`, puis `(ETH-USD, …)`.

Le `row_count` du manifeste du dataset est la somme des `row_count` des segments. Au jalon 1 les datasets sont mono-série (`btcusd_2y` : `kraken`, `BTC-USD`, `1m`), donc un seul segment.

## 6. Versionnement

Toute modification de ce format (champ d'en-tête ajouté, retiré ou reformaté, ordre des colonnes, canonicalisation, encodage de `trades`, ordre des segments, fonction de hachage) **crée `dsh-v2`**, avec un nouveau préfixe d'en-tête (`tradingv4-dataset-v2`) et une nouvelle valeur de `hash_format`. Les hashes `dsh-v1` existants ne sont jamais recalculés silencieusement dans le nouveau format : un manifeste garde le format avec lequel il a été produit, et un outil qui compare deux hashes de formats différents refuse la comparaison.

Le test golden de S4 (§7) protège ce document : une montée de version d'`arrow`, de `parquet` ou de `blake3` qui change le hash attendu est bloquée en CI.

## 7. Vecteur de test

Trois bougies d'une série `kraken` / `BTC-USD` / `1m`, fenêtre `[2024-01-01T00:00:00Z, 2024-01-01T00:03:00Z)`, `gaps_policy = skip`.

| # | `open_time_ms` | `open` | `high` | `low` | `close` | `volume` | `trades` |
|---|---|---|---|---|---|---|---|
| 1 | 1704067200000 | 42283.6 | 42300.0 | 42261.0 | 42290.1 | 1.25 | 17 |
| 2 | 1704067260000 | 42290.1 | 42295.5 | 42280.0 | 42281.2 | 0.75 | absent |
| 3 | 1704067320000 | 42281.2 | 42281.2 | 42281.2 | 42281.2 | -0.0 | 0 |

En-tête (74 octets) :

```
tradingv4-dataset-v1|kraken|BTC-USD|1m|1704067200000,1704067380000|skip|3\n
```

Octets d'entrée en hexadécimal, partie par partie :

```
en-tête  74726164696e6776342d646174617365742d76317c6b72616b656e7c4254432d5553447c316d7c313730343036373230303030302c313730343036373338303030307c736b69707c330a
ligne 1  00f451c28c0100003333333373a5e4400000000080a7e44000000000a0a2e4403333333343a6e440000000000000f43f0111000000
ligne 2  60de52c28c0100003333333343a6e44000000000f0a6e4400000000000a5e4406666666626a5e440000000000000e83f0000000000
ligne 3  c0c853c28c0100006666666626a5e4406666666626a5e4406666666626a5e4406666666626a5e44000000000000000000100000000
```

Flux complet (233 octets = 74 + 3 × 53), à hacher tel quel :

```
74726164696e6776342d646174617365742d76317c6b72616b656e7c4254432d5553447c316d7c313730343036373230303030302c313730343036373338303030307c736b69707c330a00f451c28c0100003333333373a5e4400000000080a7e44000000000a0a2e4403333333343a6e440000000000000f43f011100000060de52c28c0100003333333343a6e44000000000f0a6e4400000000000a5e4406666666626a5e440000000000000e83f0000000000c0c853c28c0100006666666626a5e4406666666626a5e4406666666626a5e4406666666626a5e44000000000000000000100000000
```

Hash `dsh-v1` attendu :

```
a15f886ff621e7e10b7d4769793d818b23686441a2ce3d4dd6a4918b740cae8e
```

soit `"blake3:a15f886ff621e7e10b7d4769793d818b23686441a2ce3d4dd6a4918b740cae8e"` dans un manifeste.

Points illustrés : la ligne 2 a `trades` absent (`00 00000000`), la ligne 3 a `trades = 0` (`01 00000000`) et un volume `-0.0` écrit `0000000000000000`.

Propriétés vérifiées par le programme qui a produit ce vecteur (programme jetable, hors dépôt, `blake3 = "=1.8.7"`) :

- le hash en un appel et le hash par un `Hasher` alimenté ligne par ligne sont égaux ;
- les mêmes bougies fournies dans le désordre avec un doublon exact (`3, 1, 2, 1`) donnent le même hash ;
- `+0.0` à la place de `-0.0` donne le même hash ;
- `trades = 0` à la place de `trades` absent sur la ligne 2 change le hash ;
- un NaN de charge quelconque s'encode `0x7FF8000000000000`.

Les octets du flux complet ont aussi été reproduits à l'identique en PHP 8.3 avec `pack` (§8).

**Exigence pour S4** : l'implémentation de `dsh-v1` dans `tv4/src/store/dataset.rs` reproduit ce vecteur dans un test golden (octets du flux et hash en dur), avec les mêmes propriétés que ci-dessus en tests unitaires. Ce test s'ajoute au test golden sur fixture Parquet de 03 §5.3, il ne le remplace pas.

## 8. Recalcul en PHP (jalon 2)

Sur PHP 64 bits, chaque élément du flux s'écrit avec `pack` :

| Élément | PHP |
|---|---|
| en-tête | chaîne ASCII construite par concaténation, terminée par `"\n"` |
| `open_time_ms` (i64 LE) | `pack('P', $t)` (les entiers négatifs sortent en complément à deux) |
| flottant (f64 LE) | `pack('e', $x)` après canonicalisation : `if (is_nan($x)) { $b = pack('P', 0x7FF8000000000000); } elseif ($x == 0.0) { $b = pack('e', 0.0); } else { $b = pack('e', $x); }` |
| présence de `trades` | `"\x01"` ou `"\x00"` |
| `trades` (u32 LE) | `pack('V', $n)`, ou `pack('V', 0)` si absent |

BLAKE3 n'est pas dans `hash_algos()` de PHP 8.3 (vérifié sur 8.3.6) : le choix de l'extension ou de la bibliothèque est à faire au jalon 2, avec le vecteur du §7 comme test d'acceptation. Le control plane n'a pas besoin de recalculer les hashes pour fonctionner (`tv4` reste la source de vérité, 03 §15.2) ; le recalcul sert à vérifier un manifeste de façon indépendante.

## 9. Ce qui n'est pas `dsh-v1`

Les autres hashes du projet (03 §5.3) ne suivent pas ce format :

- `config_hash` : blake3 du JSON à clés triées de la configuration du run (`strategy.id`, `strategy.version`, `params`, `fees`, `slippage_bps`, `sim_rules`, `warmup`, `min_trades`, `engine.tv4_version`). Le JSON est produit par `serde_json::to_value` puis sérialisation compacte de la `Value` (dont les objets sont triés par clé tant que la feature `preserve_order` de `serde_json` est absente), jamais par `to_string` directement sur la structure. Le rendu des flottants est celui de `serde_json` : un recalcul en PHP avec `json_encode` n'est pas garanti identique, et n'est pas prévu.
- `run_id` : blake3 de `config_hash ‖ dataset_hash ‖ window`, détaillé en S8.
- `result_hash` : blake3 des octets de `trades.csv`.
- Un hash physique de fichier (sha256 des octets Parquet), s'il est conservé pour l'intégrité de transfert, est un champ distinct, clairement nommé, et ne remplace jamais `content_hash`.
