<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Une jambe de `research.legs` (schéma s11) : début d'activité et refus d'entrée de la recherche. */
final readonly class ResearchLeg
{
    public function __construct(
        public string $symbol,
        public ?string $liveFrom,
        public ResearchFunnel $funnel,
    ) {
    }
}
