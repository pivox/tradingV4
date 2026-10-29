<?php

declare(strict_types=1);

namespace App\Run\Dto;

/**
 * Contenu de `metrics.json` (schémas `tv4-metrics-s8`, `tv4-metrics-s9`, `tv4-metrics-s10` et `tv4-metrics-s11`), sans le bloc `config`
 * repris par le manifeste. Les blocs propres au portefeuille (s9) valent null pour un run s8 ou s'ils sont absents.
 * Un run s10 est un run portefeuille qui porte en plus les blocs `costs`, `cost_arithmetic` et `cost_counters`
 * (obligatoires en s10, null avant). Un run s11 (dataset de recherche par bloc) est un run portefeuille qui porte en plus
 * le bloc obligatoire `research` ; il porte les blocs de coûts si et seulement s'il a été joué avec un profil (`--costs`).
 * `signalsRejectedMaxPositions` (s9) compte les refus du plafond de positions, inclus dans `signalsRejected`
 * mais hors de `rejectedByReason` ; null pour un run s8.
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
        public ?int $signalsRejectedMaxPositions = null,
        public ?CostsBlock $costs = null,
        public ?CostArithmetic $costArithmetic = null,
        public ?CostCounters $costCounters = null,
        public ?ResearchInfo $research = null,
    ) {
    }

    /** Vrai pour un run du mode portefeuille (schémas s9, s10 et s11). */
    public function isPortfolio(): bool
    {
        return 'tv4-metrics-s9' === $this->schema || $this->isCosts() || $this->isResearch();
    }

    /** Vrai pour un run joué avec un profil de coûts (schéma s10, ou s11 avec les blocs de coûts). */
    public function isCosts(): bool
    {
        return null !== $this->costs;
    }

    /** Vrai pour un run de recherche sur un dataset de bloc (schéma s11). */
    public function isResearch(): bool
    {
        return 'tv4-metrics-s11' === $this->schema;
    }
}
