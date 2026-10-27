<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Un run lu depuis ses quatre fichiers, prêt à être rendu. */
final readonly class RunReport
{
    /**
     * @param list<TradeRow> $trades
     */
    public function __construct(
        public Manifest $manifest,
        public Metrics $metrics,
        public array $trades,
        public EquitySeries $equity,
    ) {
    }
}
