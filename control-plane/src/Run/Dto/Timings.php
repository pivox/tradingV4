<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Durées internes du run en millisecondes (`manifest.timings`). */
final readonly class Timings
{
    public function __construct(
        public int $loadMs,
        public int $runMs,
        public int $cliColdMs,
    ) {
    }
}
