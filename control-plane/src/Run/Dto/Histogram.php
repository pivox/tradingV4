<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Histogramme des multiples de R : `counts[k]` couvre `[min + k × binWidth, min + (k + 1) × binWidth)`. */
final readonly class Histogram
{
    /**
     * @param list<int> $counts
     */
    public function __construct(
        public float $binWidth,
        public float $min,
        public float $max,
        public array $counts,
        public int $underflow,
        public int $overflow,
    ) {
    }
}
