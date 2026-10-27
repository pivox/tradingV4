<?php

declare(strict_types=1);

namespace App\Report;

use App\Report\Chart\DrawdownChart;
use App\Report\Chart\HistogramChart;
use App\Report\Chart\LineChart;
use App\Report\Chart\ScatterChart;
use App\Run\Dto\RunReport;
use Twig\Environment;

/**
 * Rend le rapport HTML autonome d'un run. Rendu déterministe : aucune date de génération, aucun
 * chemin absolu ; deux rendus du même run sont identiques à l'octet.
 */
final class ReportRenderer
{
    /** Nombre maximal de lignes du tableau des trades ; la suite est renvoyée à `trades.csv`. */
    public const MAX_TRADE_ROWS = 2000;

    public function __construct(
        private readonly Environment $twig,
        private readonly Format $format,
        private readonly LineChart $lineChart,
        private readonly DrawdownChart $drawdownChart,
        private readonly ScatterChart $scatterChart,
        private readonly HistogramChart $histogramChart,
    ) {
    }

    public function render(RunReport $run): string
    {
        $equity = $run->equity;
        $histogram = $run->metrics->distribution->histogramR;
        $shown = array_slice($run->trades, 0, self::MAX_TRADE_ROWS);

        return $this->twig->render('report/standalone.html.twig', [
            'run' => $run,
            'manifest' => $run->manifest,
            'metrics' => $run->metrics,
            'equityChart' => $this->lineChart->build(
                $equity->timesMs,
                $equity->equity,
                fn (float $v): string => $this->format->money($v),
            ),
            'drawdownChart' => $this->drawdownChart->build($equity->timesMs, $equity->drawdown),
            'scatterChart' => $this->scatterChart->build($run->trades),
            'histogramChart' => null === $histogram ? null : $this->histogramChart->build($histogram),
            'shownTrades' => $shown,
            'hiddenTrades' => count($run->trades) - count($shown),
            'windowStart' => $this->format->dateTime($run->manifest->window->startMs),
            'windowEnd' => $this->format->dateTime($run->manifest->window->endMs),
        ]);
    }
}
