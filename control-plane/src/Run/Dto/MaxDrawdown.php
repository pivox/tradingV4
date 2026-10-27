<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Plus grand drawdown d'une série : fraction du pic, montant, instants du pic et du creux. */
final readonly class MaxDrawdown
{
    public function __construct(
        public float $pct,
        public float $quote,
        public ?int $peakMs,
        public ?int $troughMs,
    ) {
    }
}
