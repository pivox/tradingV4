<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Résultat d'une paire (`metrics.distribution.by_pair[]`, schéma s9). */
final readonly class PairStat
{
    public function __construct(
        public string $symbol,
        public int $trades,
        public ?float $winRate,
        public ?float $expectancyR,
        public float $netPnl,
        public ?float $exposure,
    ) {
    }
}
