<?php

declare(strict_types=1);

namespace App\Report\Chart;

use App\Report\Format;

/** Drawdown sous l'equity : aire entre la courbe (≤ 0) et la ligne zéro, un axe pour lui seul. */
final class DrawdownChart
{
    public function __construct(
        private readonly LineChart $line,
        private readonly Format $format,
    ) {
    }

    /**
     * @param list<int>   $timesMs
     * @param list<float> $drawdown fractions négatives ou nulles
     */
    public function build(array $timesMs, array $drawdown, ?Plot $plot = null): LineGeometry
    {
        return $this->line->build(
            $timesMs,
            $drawdown,
            fn (float $v): string => $this->format->percent($v),
            0.0,
            $plot ?? new Plot(height: 140.0),
        );
    }
}
