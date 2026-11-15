<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Provenance déclarée par le moteur, sans recalcul côté présentation. */
final readonly class NoiseContext
{
    public function __construct(
        public int $n,
        public ?int $nEff,
        public float $selectionYears,
        public ?float $expected,
        public ?float $approxQ90,
        public ?float $q95,
    ) {
    }
}
