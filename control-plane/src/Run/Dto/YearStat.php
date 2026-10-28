<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Résultat d'une année UTC (année de sortie des trades). */
final readonly class YearStat
{
    public function __construct(
        public int $year,
        public int $nTrades,
        public int $nWins,
        public float $pnlNet,
    ) {
    }
}
