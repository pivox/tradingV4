<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Part de la fenêtre passée en position. */
final readonly class Exposure
{
    public function __construct(
        public int $barsInPosition,
        public int $barsInWindow,
        public float $fraction,
    ) {
    }
}
