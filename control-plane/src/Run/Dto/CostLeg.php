<?php

declare(strict_types=1);

namespace App\Run\Dto;

/**
 * Paramètres de coût résolus pour une jambe (`metrics.costs.legs[]`, schéma `tv4-metrics-s10`), avec leur
 * source : `list` ou `fallback` pour la classe, `default` ou `measured` pour le spread et le slippage.
 * Les taux sont en points de base (bp), sauf les probabilités et la chute de stress (fractions).
 */
final readonly class CostLeg
{
    /**
     * @param array{p50: float, p95: float, p99: float, p100: float} $stopSlippageBps nœuds de la loi du slippage de stop
     */
    public function __construct(
        public string $symbol,
        public string $class,
        public string $classSource,
        public string $spreadSource,
        public string $slippageSource,
        public float $halfSpreadBps,
        public float $marketSlippageBps,
        public ?float $measuredP95HalfSpreadBps,
        public array $stopSlippageBps,
        public float $stopSlippageMeanBps,
        public float $stressProbability,
        public float $stressDrop,
        public string $entry,
        public ?float $makerFillProbability,
        public ?int $makerTtlMinutes,
        public float $adverseSelectionBps,
    ) {
    }
}
