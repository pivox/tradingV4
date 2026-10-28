<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Entonnoir des signaux (`metrics.funnel`). */
final readonly class Funnel
{
    /**
     * @param array<string, int> $entryFilteredByReason
     * @param array<string, int> $entryRejectedByReason
     * @param array<string, int> $entryNotFilledByReason
     * @param array<string, int> $exitsByType
     */
    public function __construct(
        public int $evaluations,
        public ?int $rawEntrySignals,
        public array $entryFilteredByReason,
        public int $entrySignals,
        public array $entryRejectedByReason,
        public int $entryOrders,
        public array $entryNotFilledByReason,
        public int $entryFills,
        public int $trades,
        public int $exitSignals,
        public int $exitOrders,
        public int $otherSignals,
        public int $otherAccepted,
        public array $exitsByType,
    ) {
    }
}
