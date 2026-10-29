<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Refus d'entrée propres au mode recherche (docs/specs/s11.md §8.3), au total ou pour une jambe. */
final readonly class ResearchFunnel
{
    public function __construct(
        public int $entriesOutsideDecisionWindow,
        public int $entriesNotInUniverse,
        public int $entriesExcludedQuality,
        public int $entriesCancelledUniverse,
    ) {
    }

    public function total(): int
    {
        return $this->entriesOutsideDecisionWindow + $this->entriesNotInUniverse + $this->entriesExcludedQuality;
    }
}
