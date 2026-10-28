<?php

declare(strict_types=1);

namespace App\Report\Chart;

/** Cadre commun des graphiques : taille du viewBox et zone de tracé. */
final readonly class Plot
{
    public function __construct(
        public float $width = 720.0,
        public float $height = 240.0,
        public float $left = 72.0,
        public float $top = 10.0,
        public float $right = 12.0,
        public float $bottom = 28.0,
    ) {
    }

    public function plotWidth(): float
    {
        return $this->width - $this->left - $this->right;
    }

    public function plotHeight(): float
    {
        return $this->height - $this->top - $this->bottom;
    }

    public function xRight(): float
    {
        return $this->width - $this->right;
    }

    public function yBottom(): float
    {
        return $this->height - $this->bottom;
    }
}
