<?php

declare(strict_types=1);

namespace App\Report\Chart;

/** Graduation d'axe : position en unités du viewBox SVG et étiquette. */
final readonly class Tick
{
    public function __construct(
        public float $pos,
        public string $label,
    ) {
    }
}
