<?php

declare(strict_types=1);

namespace App\Run\Dto;

/**
 * Compteurs de coûts (`metrics.cost_counters`, schéma `tv4-metrics-s10`) : la somme des jambes
 * (`symbol` nul, `byPair` rempli) ou une paire seule (`symbol` rempli, `byPair` vide).
 * `makerEntryMakerShare` est la part des entrées converties en limit et remplies en maker, nulle sans entrée convertie.
 */
final readonly class CostCounters
{
    /**
     * @param list<CostCounters> $byPair
     */
    public function __construct(
        public ?string $symbol,
        public int $stopFillsDrawn,
        public int $stopStressFills,
        public int $makerEntriesConverted,
        public int $makerEntriesTakenAtArrival,
        public int $makerFillDraws,
        public int $makerFills,
        public int $makerFillMissed,
        public int $makerEntriesFilledMaker,
        public float $spreadCostTotal,
        public float $adverseSelectionTotal,
        public ?float $makerEntryMakerShare,
        public array $byPair = [],
    ) {
    }
}
