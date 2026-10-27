#!/usr/bin/env bash
# Porte criterion (docs/03-architecture.md §8, spécification S8 §11.2).
#
# Usage : criterion-gate.sh <estimates.json> <éléments> <seuil_ns>
#
#   estimates.json  target/criterion/<groupe>/<fonction>/new/estimates.json
#   éléments        nombre de 1m d'une itération (Throughput::Elements), 1051200 pour engine_2y_1m
#   seuil_ns        seuil en nanosecondes par 1m, 50 pour la boucle de sma_cross 15m
#
# Lit median.point_estimate et median.confidence_interval.lower_bound (nanosecondes par itération, une
# itération = une boucle de <éléments> 1m), divise par <éléments>, écrit le résumé JSON dans
# $BENCH_SUMMARY (défaut : bench-summary.json) et échoue (code 1) si lower_bound / éléments >= seuil.
# La borne basse, pas la médiane : sur un runner partagé la médiane bouge de quelques dizaines de pour
# cent d'un run à l'autre ; la borne basse ne dépasse le seuil que si même la meilleure estimation est
# au-dessus, c'est-à-dire pour une régression grossière. Elle ne prouve pas la performance absolue.
#
# Codes : 0 sous le seuil, 1 seuil dépassé, 2 usage ou fichier illisible. Dépendances : jq, awk.
set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage : $0 <estimates.json> <éléments> <seuil_ns>" >&2
  exit 2
fi
estimates=$1
elements=$2
threshold=$3

if [ ! -r "$estimates" ]; then
  echo "erreur : estimations illisibles : $estimates" >&2
  exit 2
fi
if ! [[ "$elements" =~ ^[1-9][0-9]*$ ]]; then
  echo "erreur : <éléments> doit être un entier strictement positif : $elements" >&2
  exit 2
fi
if ! [[ "$threshold" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
  echo "erreur : <seuil_ns> doit être un nombre positif : $threshold" >&2
  exit 2
fi

# <groupe>/<fonction>/new/estimates.json : le nom du bench est le dossier au-dessus de `new`.
bench=$(basename "$(dirname "$(dirname "$estimates")")")
median=$(jq -er '.median.point_estimate' "$estimates")
lower=$(jq -er '.median.confidence_interval.lower_bound' "$estimates")

per_bar() { awk -v v="$1" -v n="$elements" 'BEGIN { printf "%.6f", v / n }'; }
median_per_bar=$(per_bar "$median")
lower_per_bar=$(per_bar "$lower")

summary=${BENCH_SUMMARY:-bench-summary.json}
jq -n \
  --arg bench "$bench" \
  --argjson elements "$elements" \
  --argjson median "$median_per_bar" \
  --argjson lower "$lower_per_bar" \
  --argjson threshold "$threshold" \
  '{bench: $bench, elements: $elements, median_ns_per_bar: $median,
    lower_bound_ns_per_bar: $lower, threshold_ns_per_bar: $threshold}' > "$summary"

echo "$bench : médiane $median_per_bar ns/1m, borne basse $lower_per_bar ns/1m, seuil $threshold ns/1m"
if awk -v l="$lower_per_bar" -v t="$threshold" 'BEGIN { exit !(l >= t) }'; then
  echo "ÉCHEC : la borne basse de la médiane ($lower_per_bar ns/1m) atteint le seuil ($threshold ns/1m)" >&2
  exit 1
fi
echo "OK : la borne basse de la médiane est sous le seuil"
