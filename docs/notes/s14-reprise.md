# S14 — reprise après le checkpoint intermédiaire

## Périmètre de cette PR

Le propriétaire a demandé un checkpoint et une PR intermédiaire avec ce document
pour reprendre plus tard. Cette livraison termine les **lots 1 à 11** du
[plan détaillé](../superpowers/plans/2026-11-15-s14-controls.md).
L’[issue S14 #16](https://github.com/pivox/tradingV4/issues/16) reste ouverte.
Les lots 12 à 14 ne sont pas implémentés et aucune calibration S14 ou mesure
27×200 n’est annoncée. Après revue, validations et fusion de cette PR, le travail
s’arrête ; la prochaine reprise commence au lot 12.

Le socle provient de `main` après S13 (`400b895`). Les commits déjà présents dans
la branche incluent la conception (`c05468a`), le socle numérique (`85fb7de`), le
pas natif/préparation (`22d1793`) et les rejeux/statistiques/S12 (`5e7cbd0`).
Pour poursuivre après fusion, repartir du `main` mis à jour dans une nouvelle
branche ; ne pas réutiliser un ancien binaire pour des sources d’un autre moteur.

## Ce qui fonctionne

- Géométrie intervenue, sondes natives et vrais rejeux du Book ; dataset partagé
  par Arc, conversion unique et préparation partagée quand les déclarations
  sont identiques. Le chemin normal du moteur conserve ses goldens et allocations.
- Quotas par paire/année UTC du fill, distinction rempli/conservé/purgé,
  comptage exact BigUint par DP et tirages uniformes avec SplitMix64 explicite.
  La première tentative conforme est conservée, avant toute inspection du PnL.
- Maxima de N contrôles par réplication, p-values conservatrices avec correction
  +1, rang critique entier, contraste apparié, erreur Monte Carlo et bootstrap
  mensuel commun B = 10000. Une matrice requise incomplète interdit ses p-values.
- Identités des programmes et comptage S12 inclusif : les seeds nulles comptent
  dans N ; les aliases et le contrôle apparié réutilisent leurs configurations.
  Famille réservée immuable, aucun null promouvable en candidat.
- CLI `tv4 controls`, plan et recettes possédés/commitables, préflight avant
  Parquet, admission unique, Source Observed aux trois empreintes strictes,
  publication exclusive d’un bundle neuf. `tv4 verify` et `tv4 stats` reconnaissent
  les bundles et projections tout en conservant les layouts antérieurs.

La conception normative est [docs/specs/s14.md](../specs/s14.md).

## Fichiers et interfaces à retrouver

| Partie | Entrées principales |
|---|---|
| Géométrie | `tv4/src/strategy/controls.rs`, `sma_cross.rs` |
| Numérique | `tv4/src/research/controls/{types,calendar,random,statistics,bootstrap}.rs` |
| Moteur | `tv4/src/engine/controls.rs`, `controls/{replay,batch}.rs`, `portfolio/{step,controls}.rs` |
| S12 | `research/registry/{roles,policy,mod}.rs`, `cli/research/admission.rs` |
| Bundle | `report/controls/{identity,types,bundle,verify}.rs`, `report/verify.rs` |
| CLI | `cli/controls/{plan,sources,replay,mod}.rs` |
| Recettes univers | `store::universe::load_from_bytes`, même validateur que le chargement historique |
| E2E et exemples de plan | `tv4/tests/cli_controls.rs`, `cli_controls_common/{helpers,fixtureplan}.rs` |
| Tests de publication | `tv4/tests/report_controls.rs` |

`engine::controls::run_batch[_with_work]` rend un `BatchOutcome` possédé.
`preflight_geometry` expose le même hash de géométrie avant données.
`report::controls::bundle::{build_bundle,publish,source_quotas}` construit les
artefacts à partir des `VerifiedRun` possédés ; `with_measurements` ajoute un
fichier non numérique avant l’index. `VerifiedRun::controls()` porte une
projection déjà chargée. Le schéma réel de tous les DTO est dans `types.rs`.

## Reproduire le checkpoint

Rust 1.97.0 est épinglé. Utiliser deux jobs pour limiter la RAM et un TMPDIR
privé sous `/var/tmp` : un marqueur `/tmp/.git` extérieur au dépôt perturbe des
tests de fausses clés ; ne pas supprimer ce marqueur.

```bash
export CARGO_BUILD_JOBS=2
export TMPDIR="$(mktemp -d /var/tmp/tv4-s14.XXXXXX)"
cargo test --locked --test cli_controls --test report_controls
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
```

Sur cette machine, le worktree de préparation est
`/home/haythem/.codex/worktrees/d823/tradingV4` et le cache partagé est
`/home/haythem/workspace/perso/tradingV4/target` (`CARGO_TARGET_DIR`). Si le wrapper
rustup échoue, les binaires directs sont sous
`/home/haythem/.rustup/toolchains/1.97.0-x86_64-unknown-linux-gnu/bin/` ; fixer aussi
`RUSTC` et `RUSTDOC` vers cette toolchain.

La commande CLI réelle est :

```bash
./target/release/tv4 controls runs/source-a \
  --peer-run runs/source-b --plan research/controls/s14.json \
  --root data --seed 42 --out runs/controls-a
./target/release/tv4 verify runs/controls-a runs/controls-a
```

Les fichiers et hashes du plan dépendent des sources. Pour un exemple exécutable
sans données du propriétaire, les tests `cli_controls` créent leur propre dépôt
Git, ingèrent une petite série synthétique, construisent le dataset et produisent
les sources avec le même binaire CLI avant les contrôles. Le plan doit garder
`symbols` comme déclaration complète du parent ; les jambes du subset sont
séparées. La fenêtre décisionnelle S11 d’admission et la fenêtre réellement
jouée après warmup sont également distinctes.

## Prochaine étape : lot 12, lecteur et rapport PHP

Aucun lecteur/DTO/Twig S14 n’a encore été ajouté. Retirer ce statut seulement
après implémentation et tests ; `tv4:report <bundle>` n’est pas livré ici.

Trois fixtures natives sont déjà sous
`control-plane/tests/fixtures/controls-rust/{smoke,complete,inconclusive}`,
produites par le vrai moteur et validées par le chargeur Rust. Régénération :

```bash
cargo test --locked --test report_controls export_php_controls_fixture -- --ignored --exact
```

Ces fixtures servent au lecteur PHP ; leurs métadonnées de moteur synthétiques
ne sont pas des sources rejouables par le CLI courant. `complete` signifie une
matrice K = 200 complète ; elle reste technique/non qualifiante, avec incertitude
temporelle `insufficient_months`. Ne pas la présenter comme un succès G1.

Raccorder des DTO readonly et validators stricts, un lecteur de projection et un
lecteur de bundle ; priorité controls avant lines/profiles/run dans la commande.
Rendre la fiche canonique explicitement nommée, les trois contrôles, disponibilité,
quotas, résolution et smoke. Conserver les autres sections et l’autoescape.
Ne recalculer ni p-values, maxima, quantiles, R ou BLAKE3 en PHP : comparer les
liens déclarés. Rejeter les fichiers/chemins S14 externes et symlinks avant lecture,
y compris les liens pendants ; absent sur un ancien run reste compatible.

Les uint64 JSON numériques doivent conserver type et précision. Une piste sans
nouveau parser est de rapprocher deux décodages du même buffer, avec puis sans
`JSON_BIGINT_AS_STRING` : un entier débordant devient string/float, une chaîne
d’origine reste string/string. Un marqueur interne peut préserver cette
information ; éviter un second arbre en mémoire pour les cas ordinaires.
La graine maître et les cardinalités BigUint sont déjà des chaînes canoniques.
Ajouter les tests de frontières avant de choisir la représentation finale PHP.

Puis exécuter `composer validate --strict`, `composer cs`, PHPStan avec limite
512M et la suite PHPUnit complète. Le vendor du worktree est installé.

## Lots 13 et 14 : preuves encore nécessaires

Le [protocole de calibration](s14-calibration.md) est commité depuis `22d1793`,
avant toute campagne : 400 martingales, N = 3, durées fixes de 3/5/7 slots, K = 200, graines maîtres
0x53313401/02/03 ; bande préfixée de 8 à 35 rejets, aucune égalité. Une campagne de
sensibilité séparée exige au moins 32 rejets sur 40. Les fichiers
`controls_calibration.rs` et `controls_common/exogenous.rs` restent à créer.
Établir les calendriers uniformes avant les prix avec le sampler exact, attester
cash/stops non liants, frais réels du Book, quotas et horloges. Ne pas modifier
les graines ou la bande après lecture des résultats. Exécution dédiée prévue :

```bash
cargo test --release --locked --test controls_calibration -- --ignored --nocapture
```

Créer ensuite le banc `tv4/benches/controls.rs` : une seule campagne release
27×200 sur un cœur autorisé, dataset synthétique short d’une paire sur deux ans de données 1m,
décision 4h et 20 lots conservés/an. Publier 5400 acceptations distinctes des
exécutions tentées, phases, bougies et RSS ; utiliser le PhaseObserver extérieur
au calcul. Mesurer séparément le vrai retrait BTC, sans compter l’alias SMA
comme un second coût. Le preset 12 paires × 8,5 ans est optionnel ; le petit banc
ne démontre pas la projection de 30 minutes. Adapter CI/docs seulement à cette étape,
puis validation globale, revue et PR finale qui clôturera #16.

## Invariants à préserver

- Book utilise le C0 exact du run. La tolérance du paquet S13 à une ULP ne doit
  pas modifier le capital simulé ni relâcher les trois hashes Observed.
- Bootstrap sur tous les mois représentant fenêtre/journal, y compris bords et
  exclusions ; deux mois Complete sont une condition, pas un filtre. Temporal
  unavailable reste distinct de matrix inconclusive ; aucun zéro pour R inconnu.
- BigUint du support conservé pendant DP : zéro prouvé, None inconnu ; ne pas
  refaire DP dans report/PHP. Aucun fallback calendrier ni quota ajouté.
- K de 1 à 1000 en CLI, défaut 200, K<200 smoke ; identités des premiers programmes
  stables à l’extension K et à l’inversion des peers. N du registre inclut nulls.
- Pas de séries avant admission ; DEV effectif ou None explicitement technique.
  Avant finish, erreur technique termine les réservations en failed. Après
  finish, publication en erreur rend le code 1, conserve la lecture et les résultats
  terminés ; aucun rollback CSV/filesystem ni bundle partiel déclaré complet.
- Ne lire aucune clé, `~/.kraken` ou `.env`, et ne lancer aucune campagne réelle
  ni ordre exchange pour terminer ces trois lots.

## Preuves disponibles

Le socle jusqu’à `5e7cbd0` a passé 1770 tests sur 94 suites, fmt/Clippy toutes cibles,
ainsi que [les quatre jobs GitHub](https://github.com/pivox/tradingV4/actions/runs/43711541054).
Cette CI mesure 95,018 % de lignes couvertes et 22,749 ns/bougie (borne 22,734 < 50).
Au checkpoint 1–11 après revue et corrections, les vérifications globales
passent : **1819 tests Rust sur 96 suites**, deux générateurs ignorés, fmt/Clippy
toutes cibles et bancs en mode test ; **453 tests PHP et 2621 assertions**,
Composer/CS/PHPStan verts. Une revue experte indépendante et le contrôle
mécanique ont été effectués ; la présence et la cohérence des statistiques
Monte Carlo ont été renforcées. La couverture locale de ce checkpoint passe à **94,889 %**
(41602/43843 lignes). Les contrôles GitHub du head de PR figurent dans sa
section de vérification. Les commandes et preuves ciblées
sont également consignées dans le plan.
Aucune de ces preuves ne remplace la calibration ou le benchmark S14 restant.
