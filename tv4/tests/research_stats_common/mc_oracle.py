"""Oracle S13 indépendant : trajectoires factorisées exhaustives et quantiles type 7.
Exécution : python3 tv4/tests/research_stats_common/mc_oracle.py
A et B durent exactement un an. Aucun RNG Rust ni partition Rust recopié.
"""
from itertools import product
from math import floor


def percentile(values, p):
    values = sorted(values)
    h = (len(values) - 1) * p
    j = floor(h)
    return values[j] + (h - j) * (values[min(j + 1, len(values) - 1)] - values[j])


paths = []
for sequence in product(((0.5, 2.2), (0.5, 8.0)), repeat=2):
    equity = peak = 100.0
    loss = ratio = 0.0
    for segment in sequence:
        for factor in segment:
            equity *= factor
            peak = max(peak, equity)
            loss = max(loss, (peak - equity) / 100)
            ratio = max(ratio, (peak - equity) / peak)
    paths.append((loss, ratio, equity / 100 - 1))
# Masse discrète avec 2500 exemplaires par issue : quantiles empiriques de 10000 chemins.
empirical = paths * 2500
print("paths", paths)
print("dd_initial_p95_2y", percentile([p[0] for p in empirical], 0.95))
print("dd_peak_p95_2y", percentile([p[1] for p in empirical], 0.95))
print("return_p5_2y", percentile([p[2] for p in empirical], 0.05))

# Oracle de petites tranches de l'exemple 90j, source quotidienne explicite.
source = ([100.0] * 10 + [120.0] * 10 + [119.0] * 10 + [118.0] * 10
          + [138.0] * 10 + [137.0] * 10 + [136.0] * 30 + [135.0])
intervals = (list(range(61, 91)) + list(range(1, 11)), list(range(11, 61)))
factors = []
for block in intervals:
    delta = sum(source[d] - source[d - 1] for d in block)
    factor = 1.0
    for d in block:
        factor *= source[d] / source[d - 1]
    factors.append(factor)
    print("90j duration, delta, factor", len(block), delta, factor)
print("90j factor product", factors[0] * factors[1])
