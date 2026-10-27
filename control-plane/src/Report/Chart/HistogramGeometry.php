<?php

declare(strict_types=1);

namespace App\Report\Chart;

/** Histogramme prêt à écrire en SVG. */
final readonly class HistogramGeometry
{
    /**
     * @param list<HistogramBar> $bars
     * @param list<Tick>         $xTicks
     * @param list<Tick>         $yTicks
     */
    public function __construct(
        public Plot $plot,
        public array $bars,
        public array $xTicks,
        public array $yTicks,
        public ?float $zeroX,
    ) {
    }
}
