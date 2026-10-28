<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Verdict d'exécutabilité G0 d'une paire (`metrics.executability.pairs[]`, schéma s9). */
final readonly class ExecutabilityPair
{
    public function __construct(
        public string $symbol,
        public ?string $okxInstId,
        public int $entryOrders,
        public ?float $notionalRef,
        public ?float $medianQuoteVolume1m,
        public ?float $volumeCap,
        public ?float $minSz,
        public ?float $medianClose,
        public ?float $minOrderNotional,
        public ?string $state,
        public string $verdict,
    ) {
    }
}
