# Note pour 03-architecture et 04-protocole : setups déclaratifs en TOML (repli S33)

Date : 2026-10-14. Décisions de Haythem dans le projet : « ajoute-le comme un point de fallback » (08:05), puis « ok TOML, fais une proposition » (08:10). Issue : S33 (#37), **conditionnelle** : elle ne se déclenche que si le cycle 1 (S18, S19) ne produit aucun candidat validé, et sur décision du propriétaire. Statut de cette note : proposition validée point par point par Haythem le 14/10 (08:10 à 08:41, §11) ; dérogation actée le 28/10 (21:08 UTC, « acte dès maintenant ») et intégrée dans 04 §4.2, 04 §10 décision 10, 03 §6 et §13 ; recopiée dans `docs/notes/declarative-setup-toml.md` ; rien n'est codé. Destination finale : `docs/declarative-setup.md` du dépôt quand S33 s'ouvre.

## 1. Ce que c'est, et ce que ce n'est pas

Un setup déclaratif est un fichier TOML qui assemble des briques déjà codées en Rust : les indicateurs du catalogue (03 §2, `indicators`), des comparaisons, des listes `all` / `any`, un stop k × ATR14. Un petit interpréteur le lit, le valide contre les bornes du protocole, le compile une fois en arbre de nœuds typés et l'évalue à chaque clôture du timeframe de décision. Il permet de tester plus vite des variantes des familles existantes (F1, F2, F4, F5) sans écrire une structure Rust par variante.

Ce n'est **pas** un moteur de script : aucune boucle, aucune variable, aucune fonction définie par l'utilisateur, aucun indicateur nouveau. Rhai, wasmtime et PyO3 restent écartés (03 §3.2, §13 point 2). Une logique qui demande une brique absente du catalogue reste du Rust. Et ce n'est pas le « YAML à 10 seuils » de V1 et V3 (04 §4.2) : le validateur impose les mêmes bornes qu'au code Rust, et chaque variante compte un essai au registre.

## 2. Le fichier (`schema = 1`)

Emplacement (acté par Haythem le 14/10 (08:40)) : `research/setups/<name>.toml`, commité avec la fiche `H-NNN.md` qu'il sert (03 §4 : `research/` porte déjà fiches, registre et journal). Le run en garde une copie dans `runs/<run_id>/setup.toml`.

| Clé | Type | Obligatoire | Rôle et bornes |
|---|---|---|---|
| `schema` | entier | oui | Version du schéma, `1`. Toute autre valeur est refusée |
| `name` | chaîne `[a-z0-9_]{3,40}` | oui | Identifiant du setup ; la stratégie s'appelle `declarative:<name>` |
| `family` | `"F1"` … `"F6"` | oui | Famille de 04 §4.3. F0 n'est pas déclarative (§4) ; F3 reste rejetée par G0 |
| `fiche` | `"H-NNN"` | non | Renvoi vers la fiche ; facultatif pour `tv4 setup validate` et les tests de parité, obligatoire pour tout run sur DEV (CLAUDE.md, 04 §4.3 et §4.4) |
| `tf` | `"1d"` ou `"4h"` | oui | Timeframe de décision (04 §3.3 : seuls 1d et 4h sur OKX ; `"1h"` refusé au cycle 1). Même sérialisation que `Timeframe` dans les params Rust (à aligner avec S1) |
| `[params.<nom>]` | table | non | ≤ 4 paramètres **libres**. `value` (a priori, « valeur de manuel ») obligatoire ; `grid` (liste de 1 à 3 valeurs, contenant `value`) facultative. Un paramètre sans `grid`, ou à `grid` d'une valeur, n'est pas libre. Produit des tailles de `grid` ≤ 27 |
| `[entry] all` | liste de conditions | oui | **≤ 2 conditions** explicites ; le filtre de régime (§4) est la troisième. `any` est interdit à l'entrée : une alternative d'entrée est une seconde fiche |
| `[entry] order` | `"market"` (défaut) ou `"limit"` | non | `limit` = entrée maker au prix `price` (expression), TTL fixé par le protocole (§4) |
| `[entry] price` | expression | si `order = "limit"` | Prix de la limit, recalculé à chaque clôture |
| `[exit] any` | liste de conditions | oui | Sorties de signal, ≤ 3 ; liste vide admise si la famille ne sort que par stop et time-stop |
| `[stop] k` | nombre ou `"$param"` | oui | Stop initial = clôture − k × ATR14, k ∈ [1,5 ; 3] (04 §4.2, §3.3). La période 14 n'est pas réglable |
| `[stop] trailing` | booléen, défaut `false` | non | `true` = stop remonté à (plus haut des clôtures depuis l'entrée − k × ATR14) par `MoveStop`, jamais abaissé. Changer cette valeur est une seconde fiche, pas un paramètre |

Toute clé inconnue est refusée (`deny_unknown_fields`) : `score`, `weight`, `pair`, `time_stop_days`, `size`, `leverage`, `tf2` ne peuvent pas apparaître.

## 3. Grammaire des expressions

Une condition est une comparaison entre deux expressions, ou un prédicat de croisement. Une seule comparaison par chaîne : pas de `and` / `or` dans une condition, c'est ce que comptent les listes `all` / `any`.

```
condition := expr cmp expr
           | "cross_above" "(" expr "," expr ")"      # expr1 passe au-dessus de expr2 sur cette clôture
           | "cross_below" "(" expr "," expr ")"
cmp       := ">" | "<" | ">=" | "<="                  # pas d'égalité sur des flottants
expr      := term (("+" | "-") term)*
term      := factor (("*" | "/") factor)*
factor    := nombre | "$" ident | serie | "(" expr ")"
serie     := "close" | "open" | "high" | "low" | "volume"
           | "sma" "(" n ")" | "ema" "(" n ")" | "rsi" "(" n ")" | "atr" "(" n ")" | "vwap" "(" ")"
           | "highest" "(" n ")" | "lowest" "(" n ")"
n         := entier 1..400 | "$" ident                 # 400 couvre SMA 200 et ATR 100 avec marge
```

Sémantique, fixée pour que deux lecteurs lisent la même chose :

- Toutes les séries sont celles du timeframe `tf`, à la dernière bougie **clôturée** (03 §6 : `BarContext` ne donne jamais le futur). Une seule TF par setup : le multi-timeframe à score est abandonné (CLAUDE.md) et le filtre de régime, seul élément 1d d'un setup 4h, est fourni par le moteur (§4).
- `highest(n)` / `lowest(n)` = plus haut des `high` / plus bas des `low` sur les **n bougies qui précèdent** la bougie courante, bougie courante exclue. Sinon `close > highest(55)` serait presque toujours faux. Ce sont deux `IndicatorKind` à ajouter au catalogue (`Highest { period }`, `Lowest { period }`), dont F1 a besoin de toute façon. Acté par Haythem le 14/10 (08:40).
- `cross_above(a, b)` vaut vrai si `a > b` à cette clôture et `a <= b` à la précédente ; c'est la sémantique du `sma_cross` de 03 §6 (`prev_diff`). `a > b` seul est une condition d'**état** : elle réentre après un stop tant que l'état dure, là où le croisement attend un nouveau croisement. Les deux formes sont permises ; la fiche dit laquelle.
- Toute comparaison dont un opérande est NaN (warmup) vaut faux ; une division par zéro vaut NaN. Le moteur refuse de démarrer si un indicateur est encore NaN à `from` (03 §7.1).
- `$ident` renvoie à `[params.ident]` ; un paramètre non déclaré ou déclaré et jamais utilisé est une erreur.

## 4. Ce que le protocole fixe, et que le fichier ne peut pas changer

- **Filtre de régime** : BTC clôture 1d > SMA200, condition d'entrée de toute famille, comptée pour une des ≤ 3 conditions ; il bloque les entrées et ne ferme pas les positions (04 §4.2, décision du 13/10 (18:35)). L'interpréteur l'applique sans qu'il figure dans le fichier. Il doit être fourni par le moteur à toute stratégie, quel que soit le symbole (série BTC 1d et sa SMA200 dans `BarContext`) : c'est une capacité de S9, nécessaire aux familles Rust de S18 avant d'être nécessaire ici (acté par Haythem le 14/10 (08:41), point inscrit dans l'issue S9, #11).
- **F0 reste en Rust** : poids égal BTC/ETH et sortie au retournement du filtre ne tiennent pas dans ce schéma, et le témoin n'a pas à être facile à faire varier (choix laissé au rédacteur par Haythem le 14/10 (08:41), maintenu).
- **Time-stop 30 jours** (04 §2.3, §4.2) : l'interpréteur émet `ExitLong { Market, tag: "time_stop" }` quand `now_ms − position.opened_at_ms ≥ 30 j`. L'exception F3 n'existe pas ici.
- **Sizing** : `Size::RiskPct` au risque fixé par le mode du run (1 % du capital initial en recherche, 1 % de l'equity courante en paper et live, 04 §4.2 et §9 point 6) ; jamais dans le fichier.
- **Pas de pyramidage** : `EnterLong` n'est émis que sans position ni ordre d'entrée en attente ; le simulateur compte de toute façon le rejet (03 §7.2).
- **TTL d'une limit d'entrée** : 1 bougie du TF de décision en 1d, 60 min en 4h (04 §3.1 point 2), converti en bougies 1m par l'interpréteur.
- **Règles de fill, slippage, frais, minimums** : au simulateur et au `CostProfile` (S6, S9, S10), jamais au setup.

## 5. Validation : ce qui est refusé avant tout run

| Règle | Message (cite la source) |
|---|---|
| `len(entry.all) > 2` | « 3 conditions d'entrée au plus, filtre BTC > SMA200 compris (04 §4.2) » |
| paramètres libres > 4 | « ≤ 4 paramètres libres (04 §4.2) » |
| une `grid` de plus de 3 valeurs, ou ne contenant pas `value` | « ≤ 3 valeurs par paramètre, dont la valeur a priori (04 §4.2) » |
| produit des grilles > 27 | « ≤ 27 variantes énumérées (04 §4.3) » |
| `k` hors [1,5 ; 3], ou une valeur de sa `grid` hors de l'intervalle | « stop = k × ATR14, k ∈ [1,5 ; 3] (04 §3.3) » |
| `tf` autre que `1d` / `4h` | « 1d et 4h seulement sur OKX (04 §3.3, §10 décision 2) » |
| `any` à l'entrée, `and` / `or` dans une condition, égalité `==` | « une condition = une comparaison ; l'alternative est une seconde fiche » |
| clé inconnue (`score`, `weight`, `pair`, `time_stop_days`, …) | « clé inconnue : le schéma 1 n'a pas de score ni de pondération (CLAUDE.md) » |
| identifiant ou fonction inconnu, période hors 1..400, `$param` non déclaré ou inutilisé | message nommant le jeton et la position |
| `order = "limit"` sans `price`, ou `price` avec `order = "market"` | « `price` va avec `order = "limit"` » |
| `family = "F0"` ou `"F3"` | « F0 reste en Rust ; F3 est rejetée par G0 (08 §A.6) » |

Le validateur imprime aussi le triplet des degrés de liberté, repris en tête de chaque rapport : conditions d'entrée = `len(entry.all) + 1`, paramètres libres = nombre de `grid` à plus d'une valeur, variantes = produit des tailles de `grid`.

## 6. Exécution

- `strategy/declarative.rs` : `parse` (crate `toml` 1.1.8, déjà retenue, 03 §3.1) → `validate` (§5) → `compile` en un `Vec<Node>` indexé (opérandes, opérateurs, prédicats) et une liste d'`IndicatorSpec`, une fois à la construction. `on_bar` ne fait qu'évaluer l'arbre par indices : aucune `String`, aucune `HashMap`, aucune allocation par bougie (03 §7.2, test allocateur compteur de S3). Les noms d'indicateurs ne sont pas des `&'static str` : l'accès passe par l'index dans l'ordre de `indicators()` (`ind_at`, écart d'implémentation S2 de 03 §6).
- Appel seulement sur les clôtures de `tf` (`just_closed`) : en 1d, 1 appel sur 1 440 bougies 1m ; le coût par bougie 1m est celui de la vérification du masque. Le seuil de 50 ns/bougie (03 §8) n'est pas en jeu, mais le bench criterion existe quand même : setup `sma_cross` déclaratif contre `sma_cross` Rust, surcoût publié.
- Signaux émis, avec des tags fixes : `EnterLong { size: RiskPct, kind: Market | Limit { price, ttl_bars }, stop: Some(close − k × atr14), take_profit: None, tag: "entry" }`, `ExitLong { Market, tag: "exit" }`, `ExitLong { Market, tag: "time_stop" }`, `MoveStop { stop }` si `trailing`. Aucun `PlaceLimit` / `CancelSlot` : les limits multiples restent aux stratégies Rust (03 §15.3).
- Registre (03 §6) : une entrée `"declarative"` dans le `match`, qui lit le fichier désigné par les params ; `Strategy::id()` = `"declarative"`, `version()` = `"schema-1"`, `params_json()` = le setup canonique complet (§7). Règle de dépendance (03 §2) : le parseur et l'évaluateur vivent dans `strategy`, qui ne dépend que de `model` ; la lecture du fichier est dans `cli`.

## 7. Hashes, registre, degrés de liberté

- Canonicalisation : TOML → structure typée → expressions re-sérialisées depuis l'arbre (espaces et parenthèses redondantes disparaissent) → JSON à clés triées (`hash.rs` de S1) → blake3.
- `setup_hash` = hash du setup **sans** les valeurs des paramètres (structure, noms, grilles) : c'est le `version_hash` du lineage de 05 §4, l'équivalent du code.
- `config_hash` = formule existante (04 §9 point 2, 03 §7.4 : stratégie, version, `params_json`, grille de frais, spreads, `sim_rules`, commit du moteur) ; `params_json` contenant le setup complet, une valeur changée change le hash. Deux fichiers équivalents à l'espace et à l'ordre des clés près donnent le même hash.
- Registre (S12) : chaque variante d'une grille est une ligne de `research/trials.csv` avec son `config_hash` ; `famille` vient du fichier ; `fiche_sha256` exige le champ `fiche`. Le triplet (N, N_eff, Sharpe de bruit q95) les compte comme n'importe quel essai : c'est la seule parade au risque signalé le 14/10 (plus les setups sont faciles, plus N monte ; à N = 250 sur 5 ans, le Sharpe de bruit vaut 1,27 en espérance et 1,57 au q95, 04 §4.4 et §7).

## 8. Exemples

F2, croisement de moyennes mobiles, 1d, forme « état » de 08 §A.3 (3 paramètres libres, 3 × 3 × 2 = 18 variantes) :

```toml
schema = 1
name = "f2_sma_1d"
family = "F2"
fiche = "H-003"
tf = "1d"

[params]
fast = { value = 20, grid = [10, 20, 30] }
slow = { value = 100, grid = [50, 100, 200] }
k    = { value = 3.0, grid = [2.0, 3.0] }

[entry]
all = ["sma($fast) > sma($slow)"]

[exit]
any = ["sma($fast) < sma($slow)"]

[stop]
k = "$k"
```

La même famille en forme « croisement », qui doit donner exactement le `trades.csv` du `sma_cross` Rust de 03 §6 (test de parité de S33) :

```toml
schema = 1
name = "f2_sma_cross_1d"
family = "F2"
tf = "1d"

[params]
fast = { value = 20 }
slow = { value = 100 }

[entry]
all = ["cross_above(sma($fast), sma($slow))"]

[exit]
any = ["cross_below(sma($fast), sma($slow))"]

[stop]
k = 3.0
```

F1, cassure Donchian 55, 1d, 1 paramètre libre (k) ; la variante 4h maker ne change que `tf`, `order` et `price` :

```toml
schema = 1
name = "f1_donchian55_1d"
family = "F1"
tf = "1d"

[params]
k = { value = 3.0, grid = [2.0, 2.5, 3.0] }

[entry]
all = ["close > highest(55)"]
# variante 4h maker (seconde fiche) : tf = "4h", order = "limit", price = "highest(55)"

[exit]
any = ["close < lowest(20)"]

[stop]
k = "$k"
```

F4, compression puis cassure de volatilité, 4h maker, 2 conditions explicites + filtre = 3 :

```toml
schema = 1
name = "f4_squeeze_4h"
family = "F4"
tf = "4h"

[params]
ratio = { value = 0.6, grid = [0.5, 0.6, 0.7] }
k     = { value = 3.0 }

[entry]
all = ["atr(14) / atr(100) < $ratio", "close > highest(20)"]
order = "limit"
price = "highest(20)"

[exit]
any = ["close < lowest(10)"]

[stop]
k = "$k"
```

## 9. CLI et formulaire Symfony

- `tv4 setup validate <fichier>` : erreurs de §5 et triplet des degrés de liberté, code de retour non nul en cas de refus. C'est ce que le formulaire appelle.
- `tv4 setup schema` : exporte le schéma 1 en JSON Schema ; le formulaire Symfony (05 §4 : « paramètres générés depuis le schéma ») s'en sert pour ses FormType.
- `tv4 backtest --setup <fichier>` = `--strategy declarative --params <fichier>` ; `tv4 grid --setup <fichier>` énumère les `grid` du fichier, sans `grid.toml` séparé.
- Formulaire (optionnel, après S21) : Live Components pour les champs dépendants (famille → conditions → paramètres), génération du TOML côté PHP par un gabarit (schéma plat, aucune bibliothèque TOML en PHP), validation par `tv4 setup validate`, puis gel du fichier dans le manifeste du run avec son `setup_hash`. PHP ne parse jamais le TOML.

## 10. Dérogation pour 04 §4.2 (actée le 28/10 (21:08 UTC), intégrée dans 04 §4.2 et §10 décision 10)

Texte inséré dans 04 §4.2, à la suite de la contrainte « écrite en code typé » :

> **Dérogation : setups déclaratifs (repli S33 ; TOML acté le 14/10 (08:10), dérogation actée le 28/10 (21:08 UTC), §10 décision 10).** Une hypothèse peut être écrite dans un setup déclaratif TOML (`docs/notes/declarative-setup-toml.md`, puis `docs/declarative-setup.md` à l'ouverture de S33) à la place d'une structure Rust, à trois conditions : (1) le validateur impose les bornes de ce paragraphe (≤ 3 conditions d'entrée dont le filtre BTC > SMA200, ≤ 4 paramètres libres à ≤ 3 valeurs, ≤ 27 variantes énumérées, stop k × ATR14 avec k ∈ [1,5 ; 3], time-stop 30 jours, une seule TF hors filtre, aucun score ni pondération) et refuse tout fichier qui les dépasse ; (2) chaque variante est inscrite au registre (§4.4) avec son `config_hash` et compte dans N ; (3) la fiche `H-NNN` (§4.3) reste obligatoire, le setup n'en est que la règle d'entrée et de sortie. Le « YAML à 10 seuils » de V1 et V3 reste interdit : c'est le validateur qui le refuse, pas la bonne volonté. F0 et F3 ne sont pas déclaratives. Le repli ne s'ouvre que si le cycle 1 ne produit aucun candidat validé, sur décision du propriétaire (S33, #37).

Dans 03 §6, la phrase « une implémentation scriptée pourrait être ajoutée plus tard sans toucher au moteur ; elle n'est pas construite » devient : « une implémentation déclarative (setups TOML assemblant des briques typées, repli S33, `docs/notes/declarative-setup-toml.md`, dérogation de 04 §4.2 actée le 28/10) pourra être ajoutée sans toucher au moteur ; aucune implémentation scriptée (§3.2, §13 point 2) ». 03 §13 point 2 (« Rust pur, sans scripting ni PyO3 ») précise que le repli assemble des briques Rust typées sans interpréter de code et ne revient pas sur cette décision. 04 §10 reçoit la décision 10, qui récapitule les décisions du 14/10 et du 28/10.

## 11. Décisions et points ouverts

Décisions de Haythem (14/10) :

1. Format TOML plutôt que YAML (08:10, « ok TOML, fais une proposition ») ; `serde_yaml` est archivé depuis mars 2024, `toml` est déjà dans `Cargo.lock`.
2. Emplacement `research/setups/<name>.toml`, commité avec la fiche qu'il sert (08:40, « 1. ok »).
3. `highest(n)` / `lowest(n)` sur les n bougies qui précèdent la bougie courante ; `Highest` / `Lowest` ajoutés à `IndicatorKind` (08:40, « 2. ok »).
4. Filtre de régime fourni par le moteur, jamais écrit dans le fichier ; la série BTC 1d et sa SMA200 exposées par `BarContext` à toute stratégie sont prévues en S9 (#11) (08:41, « 3. ok »). S9 est fusionnée le 28/10 (PR #50) ; d'après CLAUDE.md, elle expose le régime en série (clôtures 1d et SMA200) et `bull()`.

Choix laissés au rédacteur de cette note par Haythem (08:41 : « je te laisse le choix » pour le point 5 ; le point 6 n'était pas une question et reste tel que proposé) :

5. F0 hors schéma (reste en Rust : poids égal BTC/ETH et sortie au retournement du filtre) ; F3 refusée par le validateur.
6. `trailing` gardé dans le schéma 1 comme booléen (changer sa valeur = seconde fiche, 04 §4.2) ; formes « état » et « croisement » toutes deux permises, la fiche nommant laquelle et, en forme « état », la réentrée après stop. Alternative écartée : retirer `trailing` du schéma 1 et n'admettre qu'une seule forme.

Reste technique, à régler à l'ouverture de S33 :

7. Sérialisation de `tf` alignée sur celle des params Rust de S1 (`"1d"` / `"4h"` ou noms de l'enum `Timeframe`).
8. Dérogation de §10 intégrée le 28/10 dans 04 §4.2, 04 §10 (décision 10), 03 §6 et §13 ; note recopiée dans `docs/notes/declarative-setup-toml.md` ; `docs/declarative-setup.md` à écrire quand S33 s'ouvre.
