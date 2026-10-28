<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Paire écartée de la liste live avec son motif (`metrics.executability.rejected[]`). */
final readonly class ExecutabilityRejection
{
    public function __construct(
        public string $symbol,
        public string $reason,
    ) {
    }
}
