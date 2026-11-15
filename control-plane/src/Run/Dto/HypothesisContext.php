<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Provenance déclarée par le moteur, sans recalcul côté présentation. */
final readonly class HypothesisContext
{
    public function __construct(
        public string $identifiant,
        public string $sha256,
    ) {
    }
}
