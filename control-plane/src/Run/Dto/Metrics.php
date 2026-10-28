<?php

declare(strict_types=1);

namespace App\Run\Dto;

/**
 * Contenu de `metrics.json` (schémas `tv4-metrics-s8` et `tv4-metrics-s9`), sans le bloc `config` repris par
 * le manifeste. Les blocs propres au portefeuille (s9) valent null pour un run s8 ou s'ils sont absents.
 */
final readonly class Metrics
{
    /**
     * @param array<string, int>  $sim
     * @param array<string, int>  $rejectedByReason
     * @param list<string>        $simDeviations
     * @param list<PairFunnel>|null $funnelByPair
     */
    public function __construct(
        public string $configHash,
        public string $datasetHash,
        public ?int $dataBars,
        public ?int $dataSyntheticBars,
        public ?int $minutesMissing,
        public ?int $windowStartIndex,
        public ?int $windowBars,
        public int $nTrades,
        public int $nTradesForced,
        public bool $forcedClose,
        public float $pnlNet,
        public float $pnlRealized,
        public float $pnlMarked,
        public float $feesTotal,
        public float $slippageTotal,
        public float $equityFinal,
        public float $cashFinal,
        public float $returnTotal,
        public int $fills,
        public array $sim,
        public int $signalsEmitted,
        public int $signalsAccepted,
        public int $signalsRejected,
        public array $rejectedByReason,
        public array $simDeviations,
        public int $minTrades,
        public string $verdict,
        public ?float $expectancyNet,
        public ?float $expectancyR,
        public int $tradesWithR,
        public int $nWins,
        public int $nLosses,
        public ?float $winRate,
        public float $grossProfit,
        public float $grossLoss,
        public ?float $profitFactor,
        public ?float $avgHoldingMs,
        public ?float $mfeMean,
        public ?float $maeMean,
        public MaxDrawdown $drawdownM1,
        public MaxDrawdown $drawdownH1,
        public Exposure $exposure,
        public Funnel $funnel,
        public Distribution $distribution,
        public string $schema = 'tv4-metrics-s8',
        public ?array $funnelByPair = null,
        public ?PortfolioInfo $portfolio = null,
        public ?PriorityInfo $priority = null,
        public ?RegimeInfo $regime = null,
        public ?InstrumentsInfo $instruments = null,
        public ?Executability $executability = null,
        public ?G1Executable $g1Executable = null,
    ) {
    }

    /** Vrai pour un run du mode portefeuille (schéma s9). */
    public function isPortfolio(): bool
    {
        return 'tv4-metrics-s9' === $this->schema;
    }
}
