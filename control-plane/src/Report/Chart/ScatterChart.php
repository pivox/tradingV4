<?php

declare(strict_types=1);

namespace App\Report\Chart;

use App\Report\Format;
use App\Run\Dto\TradeRow;

/** Nuage MFE (vertical) contre MAE (horizontal) en monnaie de cotation, un point par trade. */
final class ScatterChart
{
    public function __construct(private readonly Format $format)
    {
    }

    /**
     * @param list<TradeRow> $trades
     */
    public function build(array $trades, ?Plot $plot = null): ScatterGeometry
    {
        $plot ??= new Plot(height: 280.0);
        if ([] === $trades) {
            return new ScatterGeometry($plot, [], [], []);
        }

        $maes = array_map(static fn (TradeRow $t): float => $t->mae, $trades);
        $mfes = array_map(static fn (TradeRow $t): float => $t->mfe, $trades);
        // L'origine reste visible : un nuage tout du même côté de zéro se lit mal sans repère.
        $xScale = new Scale(min(0.0, ...$maes), max(0.0, ...$maes), $plot->left, $plot->xRight());
        $yScale = new Scale(min(0.0, ...$mfes), max(0.0, ...$mfes), $plot->yBottom(), $plot->top);

        $points = [];
        foreach ($trades as $trade) {
            $points[] = new ScatterPoint(
                $xScale->map($trade->mae),
                $yScale->map($trade->mfe),
                $trade->pnlNet > 0.0,
                $trade->lotId,
            );
        }

        $xTicks = [];
        foreach ($xScale->ticks(5) as $t) {
            $xTicks[] = new Tick($xScale->map($t), $this->format->money($t));
        }
        $yTicks = [];
        foreach ($yScale->ticks(4) as $t) {
            $yTicks[] = new Tick($yScale->map($t), $this->format->money($t));
        }

        return new ScatterGeometry($plot, $points, $xTicks, $yTicks);
    }
}
