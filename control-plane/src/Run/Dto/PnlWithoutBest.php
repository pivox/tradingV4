<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** PnL net une fois retirés les `n` meilleurs trades ; null s'il n'y a pas plus de `n` trades. */
final readonly class PnlWithoutBest
{
    public function __construct(
        public int $n,
        public ?float $pnlNet,
    ) {
    }
}
