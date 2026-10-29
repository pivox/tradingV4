<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Bloc `research.subset` (schéma s11) : sous-ensemble de paires de la ligne `okx_usdc`. */
final readonly class ResearchSubset
{
    /**
     * @param list<string> $legsExcluded jambes de l'univers que le sous-ensemble retire de la ligne
     */
    public function __construct(
        public string $kind,
        public string $subsetHash,
        public array $legsExcluded,
    ) {
    }
}
