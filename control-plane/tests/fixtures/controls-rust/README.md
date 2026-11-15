# Fixtures natives S14

Ces bundles sont générés par `tv4/tests/report_controls.rs` avec le vrai Book,
les contrôles natifs et le chargeur de bundle Rust. Aucun résultat statistique
n’est calculé à la main en PHP. Régénérer depuis la racine :

```bash
cargo test --locked --test report_controls export_php_controls_fixture -- --ignored --exact
```

- `smoke` : N = 2, K = 20, non qualifiant.
- `complete` : N = 2, K = 200, matrice complète mais voie technique ; bootstrap
  temporel indisponible (`insufficient_months`), aucun verdict G1.
- `inconclusive` : arrêt sur budget de préparation ; N/K et programmes conservés,
  support inconnu distinct de zéro, aucune p-value de porte.

Chaque racine contient index/panneau/manifeste et deux feuilles `runs/<hash>`
autonomes. Les cinq sources de chaque feuille sont des octets natifs possédés.
Les métadonnées synthétiques de moteur ne permettent pas de rejouer ces sources
avec `tv4 controls` ; les E2E `cli_controls` génèrent leurs propres sources CLI.

Le lecteur et le rendu PHP de ces fixtures restent au lot 12, après ce checkpoint.
