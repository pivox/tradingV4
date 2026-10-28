<?php

declare(strict_types=1);

namespace App\Report\Chart;

/** Un trade du nuage MFE / MAE : gagnant = cercle, perdant = carré (la couleur ne porte pas seule le sens). */
final readonly class ScatterPoint
{
    public function __construct(
        public float $x,
        public float $y,
        public bool $winner,
        public int $lotId,
    ) {
    }
}
