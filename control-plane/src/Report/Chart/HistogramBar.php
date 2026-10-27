<?php

declare(strict_types=1);

namespace App\Report\Chart;

/** Une barre de l'histogramme de R ; `loss` vaut vrai pour les seaux entièrement sous zéro. */
final readonly class HistogramBar
{
    public function __construct(
        public float $x,
        public float $y,
        public float $width,
        public float $height,
        public int $count,
        public string $range,
        public bool $loss,
    ) {
    }
}
