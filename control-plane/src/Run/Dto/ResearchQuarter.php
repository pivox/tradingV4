<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Un trimestre de `research.universe.quarters` (schéma s11) : membres de l'univers et jambes éligibles. */
final readonly class ResearchQuarter
{
    /**
     * @param list<string> $excludedQuality jambes exclues par la qualité pour le TF de décision
     */
    public function __construct(
        public string $quarter,
        public int $members,
        public int $eligibleLegs,
        public array $excludedQuality,
    ) {
    }
}
