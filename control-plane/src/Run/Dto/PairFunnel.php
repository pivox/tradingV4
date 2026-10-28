<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Entonnoir d'une paire d'un portefeuille (`metrics.funnel_by_pair[]`, schéma s9). */
final readonly class PairFunnel
{
    /**
     * @param array<string, int> $rejected rejets par motif (`min_notional`, `lot_step`, `cash`, `max_positions`, `volume`, `other`)
     */
    public function __construct(
        public string $symbol,
        public int $evaluations,
        public int $entrySignals,
        public int $entryOrders,
        public int $entryFills,
        public int $trades,
        public array $rejected,
    ) {
    }
}
