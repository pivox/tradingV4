# research/universe

Ce dossier ne contient encore aucune donnée. Il recevra, une fois le propriétaire passé sur son poste (`docs/specs/s11.md` §15), les fichiers que produisent `tv4 universe` et `tv4 dataset build --block`, copiés tels quels par le pilote de l'étape :

| Fichier ou dossier | Produit par | Contenu |
|---|---|---|
| `kraken_r1/` | `tv4 universe build` | `universe.json` (schéma `tv4-universe-v1`), un `universe_<AAAA>-Q<n>.csv` par trimestre entièrement couvert, les `eligible_<date>.csv` des dates `--at` et `instruments_kraken.toml` |
| `survivorship_kraken_r1.json` | `tv4 universe survivorship` | rapport `tv4-survivorship-v1` : motifs attendus présents ou absents, paires délistées, borne haute du début du programme de dumps |
| `okx_usdc_<AAAAMMJJ>.json` | `tv4 universe okx-subset` | liste figée `tv4-pair-subset-v1` des bases tradables sur OKX en USDC, avec son hash |
| `okx_instruments_<AAAAMMJJ>.json` | `tv4 instruments fetch` | fichier d'instruments source de la liste (`tv4-instruments-v1`, sans secret) |

Les manifestes des quatre datasets de bloc (`research_dev`, `research_validation`, `research_holdout`, `research_forward_2026q3`) vont dans `research/datasets/`, avec leurs `dataset_hash`.

Règles :

- Les fichiers d'univers ne se modifient pas à la main : un trimestre déjà figé se reproduit à l'octet ou `tv4 universe build` refuse d'écrire (`docs/specs/s11.md` §4.5). Un nouveau dump n'ajoute que des trimestres.
- Aucune clé, aucun secret : noms de paires, volumes, dates, hashes.
- Les mesures qu'ils contiennent (comptes 5 / 8 / 9, paires absentes du dump, couverture) n'entrent dans `docs/04-protocole-recherche.md` qu'après revue, comme faits datés.
