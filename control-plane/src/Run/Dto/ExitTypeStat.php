<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Résultat par type de sortie. */
final readonly class ExitTypeStat
{
    public function __construct(
        public string $exitType,
        public int $nTrades,
        public float $pnlNet,
    ) {
    }
}
