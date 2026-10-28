<?php

declare(strict_types=1);

namespace App\Report\Chart;

use App\Report\Format;
use App\Run\Dto\Histogram;

/** Histogramme des multiples de R tel que `tv4` l'a calculé (seaux, `underflow`, `overflow`). */
final class HistogramChart
{
    public function __construct(private readonly Format $format)
    {
    }

    public function build(Histogram $histogram, ?Plot $plot = null): HistogramGeometry
    {
        $plot ??= new Plot(height: 220.0);
        $bins = count($histogram->counts);
        $xScale = new Scale($histogram->min, $histogram->max, $plot->left, $plot->xRight());
        $yScale = new Scale(0.0, (float) max(1, ...$histogram->counts), $plot->yBottom(), $plot->top);

        $bars = [];
        foreach ($histogram->counts as $k => $count) {
            $lo = $histogram->min + $k * $histogram->binWidth;
            $hi = $lo + $histogram->binWidth;
            $x = $xScale->map($lo);
            $y = $yScale->map((float) $count);
            $bars[] = new HistogramBar(
                $x,
                $y,
                max(0.0, $xScale->map($hi) - $x - 1.0),
                max(0.0, $plot->yBottom() - $y),
                $count,
                sprintf('[%s ; %s[', $this->format->number($lo, 1), $this->format->number($hi, 1)),
                $hi <= 0.0,
            );
        }

        $xTicks = [];
        for ($v = ceil($histogram->min); $v <= $histogram->max && $bins > 0; $v += 1.0) {
            if (0.0 === fmod($v, 1.0) && 0.0 === fmod($v, 2.0)) {
                $xTicks[] = new Tick($xScale->map($v), $this->format->number($v, 0));
            }
        }
        $yTicks = [];
        foreach ($yScale->ticks(4) as $t) {
            if ($t === floor($t)) {
                $yTicks[] = new Tick($yScale->map($t), $this->format->number($t, 0));
            }
        }
        $zeroX = $histogram->min <= 0.0 && $histogram->max >= 0.0 ? $xScale->map(0.0) : null;

        return new HistogramGeometry($plot, $bars, $xTicks, $yTicks, $zeroX);
    }
}
