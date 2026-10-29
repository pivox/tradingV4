<?php

declare(strict_types=1);

namespace App\Report;

use App\Report\Chart\DrawdownChart;
use App\Report\Chart\HistogramChart;
use App\Report\Chart\LineChart;
use App\Report\Chart\ScatterChart;
use App\Run\Dto\LinesReport;
use App\Run\Dto\ProfilesReport;
use App\Run\Dto\RunReport;
use App\Run\Dto\TradeRow;
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
        return $this->twig->render('report/standalone.html.twig', $this->context($run));
    }

    /**
     * Rapport d'un dossier de profils de coûts : la section « Profils de coûts », puis la fiche complète du
     * profil baseline. Les fiches adverse et maker se rendent depuis leurs sous-dossiers.
     */
    public function renderProfiles(ProfilesReport $profiles): string
    {
        return $this->twig->render('report/standalone.html.twig', $this->context($profiles->baseline()) + ['profiles' => $profiles]);
    }

    /**
     * Rapport d'un dossier de lignes (docs/specs/s11.md §9.3) : la section « Deux lignes », puis la fiche complète de la
     * ligne `full` (celle de son profil baseline quand les lignes sont des dossiers de profils, précédée de la section
     * « Profils de coûts »). La fiche de la ligne `okx_usdc` se rend depuis son sous-dossier.
     */
    public function renderLines(LinesReport $lines): string
    {
        $full = $lines->full();
        $profiles = $full->profiles;
        $context = $this->context($full->referenceRun()) + ['lines' => $lines];
        if (null !== $profiles) {
            $context += ['profiles' => $profiles];
        }

        return $this->twig->render('report/standalone.html.twig', $context);
    }

    /**
     * @return array<string, mixed>
     */
    private function context(RunReport $run): array
    {
        $equity = $run->equity;
        $histogram = $run->metrics->distribution->histogramR;
        $shown = array_slice($run->trades, 0, self::MAX_TRADE_ROWS);

        return [
            'run' => $run,
            'manifest' => $run->manifest,
            'metrics' => $run->metrics,
            'equityChart' => $this->lineChart->build(
                $equity->timesMs,
                $equity->equity,
                fn (float $v): string => $this->format->money($v),
            ),
            'drawdownChart' => $this->drawdownChart->build($equity->timesMs, $equity->drawdown),
            'scatterChart' => $this->scatterChart->build($this->keptTrades($run)),
            'histogramChart' => null === $histogram ? null : $this->histogramChart->build($histogram),
            'shownTrades' => $shown,
            'hiddenTrades' => count($run->trades) - count($shown),
            'windowStart' => $this->format->dateTime($run->manifest->window->startMs),
            'windowEnd' => $this->format->dateTime($run->manifest->window->endMs),
        ];
    }

    /**
     * Trades sur lesquels portent `mfe_mean`, `mae_mean` et la distribution : en mode recherche, les trades purgés
     * (sortie `end_of_window`) restent dans `trades.csv` mais sont écartés du nuage (docs/specs/s11.md §9.2).
     *
     * @return list<TradeRow>
     */
    private function keptTrades(RunReport $run): array
    {
        if (!$run->metrics->isResearch()) {
            return $run->trades;
        }

        return array_values(array_filter(
            $run->trades,
            static fn (TradeRow $t): bool => 'end_of_window' !== $t->exitType,
        ));
    }
}
