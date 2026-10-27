<?php

declare(strict_types=1);

namespace App\Report\Chart;

/** Échelle linéaire d'un domaine de données vers un intervalle du viewBox SVG. */
final readonly class Scale
{
    public float $domainMin;
    public float $domainMax;

    /**
     * Un domaine vide (série constante) est élargi pour que la valeur tombe au milieu du tracé.
     */
    public function __construct(
        float $domainMin,
        float $domainMax,
        private float $rangeStart,
        private float $rangeEnd,
    ) {
        if ($domainMax - $domainMin <= 0.0) {
            $pad = 0.0 === $domainMin ? 1.0 : abs($domainMin) * 0.01;
            $domainMin -= $pad;
            $domainMax += $pad;
        }
        $this->domainMin = $domainMin;
        $this->domainMax = $domainMax;
    }

    public function map(float $value): float
    {
        $ratio = ($value - $this->domainMin) / ($this->domainMax - $this->domainMin);

        return $this->rangeStart + $ratio * ($this->rangeEnd - $this->rangeStart);
    }

    /**
     * Graduations « rondes » (1, 2, 5 × 10^k) comprises dans le domaine.
     *
     * @return list<float>
     */
    public function ticks(int $target = 5): array
    {
        $range = $this->domainMax - $this->domainMin;
        $raw = $range / max(1, $target);
        $magnitude = 10 ** floor(log10($raw));
        $norm = $raw / $magnitude;
        $step = match (true) {
            $norm < 1.5 => 1.0,
            $norm < 3.0 => 2.0,
            $norm < 7.0 => 5.0,
            default => 10.0,
        } * $magnitude;

        $ticks = [];
        $value = ceil($this->domainMin / $step) * $step;
        for ($i = 0; $value <= $this->domainMax + $step * 1e-9 && $i < 50; ++$i) {
            $ticks[] = round($value, 10);
            $value += $step;
        }

        return $ticks;
    }
}
