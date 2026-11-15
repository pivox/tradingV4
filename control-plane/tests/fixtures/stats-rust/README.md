# Sorties techniques Rust S13

Snapshots des sorties réelles du binaire `tv4 backtest` puis `tv4 stats --seed
18446744073709551615 --samples 10000` du 15 novembre 2026, avant l'implémentation du
lecteur PHP. Séries synthétiques uniquement, aucun résultat de recherche économique.

- `simple` : une paire ETH, 45 jours, 43 trades, deux blocs Monte Carlo.
- `profiles` : les trois profils de coûts du même jeu technique.
- `lines` : recherche FORWARD, lignes `full` et `okx_usdc`.
- `lines-profiles` : les deux lignes et leurs trois profils, avec comparaison à de
  vrais rejeux décalés de 240 minutes ; résultats complets, hors éligibilité DEV/G2.

Seuls les manifestes, métriques, journaux, courbes, descripteurs de disposition et
sidecars statistiques sont conservés tels qu'écrits. Les datasets et les paquets
`stats-inputs.json` sont volontairement absents : PHP ne suit aucun chemin externe
et recoupe les déclarations sans recalculer BLAKE3. Les empreintes `manifest_json`
restent des données d'audit. Le parse/résérialisation Rust de doubles de gouvernance
peut modifier quelques ULP ; les identités, compteurs et graines restent exacts.
