<?php

declare(strict_types=1);

namespace App\Report\Chart;

/** Courbe prête à écrire en SVG : chemin de la ligne, chemin de l'aire éventuelle et graduations. */
final readonly class LineGeometry
{
    /**
     * @param list<Tick> $xTicks
     * @param list<Tick> $yTicks
     */
    public function __construct(
        public Plot $plot,
        public string $path,
        public ?string $areaPath,
        public array $xTicks,
        public array $yTicks,
        public int $pointCount,
    ) {
    }

    public function isEmpty(): bool
    {
        return 0 === $this->pointCount;
    }
}
