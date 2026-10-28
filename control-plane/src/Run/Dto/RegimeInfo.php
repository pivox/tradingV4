<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Bloc `metrics.regime` (schéma s9) : filtre de régime lu par la stratégie. Absent si la stratégie n'en lit pas. */
final readonly class RegimeInfo
{
    public function __construct(
        public string $symbol,
        public string $tf,
        public int $smaPeriod,
        public ?int $firstReadyMs,
        public int $daysBull,
        public int $daysBear,
    ) {
    }
}
