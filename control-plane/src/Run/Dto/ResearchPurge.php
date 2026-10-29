<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Bloc `research.purge` (schéma s11, docs/specs/s11.md §3.4) : trades conservés et purgés, PnL purgé. */
final readonly class ResearchPurge
{
    public function __construct(
        public string $rule,
        public int $tradesKept,
        public int $tradesPurged,
        public float $pnlNetPurged,
    ) {
    }
}
