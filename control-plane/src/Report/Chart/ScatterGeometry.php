<?php

declare(strict_types=1);

namespace App\Report\Chart;

/** Nuage de points prêt à écrire en SVG. */
final readonly class ScatterGeometry
{
    /**
     * @param list<ScatterPoint> $points coordonnées déjà en unités du viewBox
     * @param list<Tick>         $xTicks
     * @param list<Tick>         $yTicks
     */
    public function __construct(
        public Plot $plot,
        public array $points,
        public array $xTicks,
        public array $yTicks,
    ) {
    }

    public function isEmpty(): bool
    {
        return [] === $this->points;
    }
}
