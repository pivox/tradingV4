<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Bloc `metrics.portfolio` (schéma s9) : jambes, grille 1m commune et contraintes de portefeuille. */
final readonly class PortfolioInfo
{
    public function __construct(
        public int $legs,
        public int $gridMinutes,
        public int $maxOpenPositions,
        public ?float $maxCostToInitial,
        public int $positionsCapHits,
        public int $quoteGapExits,
        public string $sizing,
        public float $initialCash,
    ) {
    }
}
