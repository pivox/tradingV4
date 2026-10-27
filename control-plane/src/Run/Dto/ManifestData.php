<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Données rejouées telles que le manifeste les décrit (`manifest.data`). */
final readonly class ManifestData
{
    public function __construct(
        public string $source,
        public ?string $dataset,
        public ?string $fixtureBlake3,
        public string $exchange,
        public string $symbol,
        public string $tf,
        public string $gapsPolicy,
        public int $bars,
        public int $syntheticBars,
        public int $firstOpenTimeMs,
        public int $lastOpenTimeMs,
    ) {
    }
}
