<?php

declare(strict_types=1);

namespace App\Run\Dto;

/**
 * Bloc `cost_arithmetic` de `metrics.json` (schéma `tv4-metrics-s10`) : l'arithmétique de
 * docs/04-protocole-recherche.md §3.3 pour le profil du run. Les montants sont en monnaie de cotation,
 * les taux en fractions. Une valeur nulle veut dire « non définie » (aucun trade, ou aucun trade à R défini).
 */
final readonly class CostArithmetic
{
    /**
     * @param array<string, float> $costBreakdown sommes des frais, du slippage et du spread (clés telles que tv4 les écrit)
     */
    public function __construct(
        public string $decisionTf,
        public bool $protocolTf,
        public ?float $roundTripCostMean,
        public ?float $roundTripCostRateMean,
        public ?float $roundTripCostRateMedian,
        public array $costBreakdown,
        public ?float $costRMean,
        public ?float $costRMedian,
        public int $tradesWithR,
        public float $costRLimit,
        public ?bool $costRPass,
        public ?float $grossExpectancy,
        public ?float $costToGross,
        public bool $costThirdPass,
        public ?float $grossMedianRate,
        public ?bool $grossMedianVsOwnCostPass,
        public ?float $tradesPerYear,
        public ?float $capitalFractionMean,
        public ?float $hurdleAnnual,
    ) {
    }
}
