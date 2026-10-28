<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Distribution des résultats, calculée par `tv4` (`metrics.distribution`). */
final readonly class Distribution
{
    /**
     * @param list<PnlWithoutBest> $pnlWithoutBest
     * @param list<YearStat>       $byYear
     * @param list<ExitTypeStat>   $byExitType
     * @param list<PairStat>|null  $byPair        présent en s9 seulement
     */
    public function __construct(
        public ?float $pnlNetMean,
        public ?float $pnlNetMedian,
        public ?float $rMean,
        public ?float $rMedian,
        public ?Histogram $histogramR,
        public array $pnlWithoutBest,
        public array $byYear,
        public array $byExitType,
        public ?array $byPair = null,
    ) {
    }
}
