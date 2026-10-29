<?php

declare(strict_types=1);

namespace App\Run\Dto;

/**
 * Survivance résiduelle d'un profil (`residual_survivorship.<profil>` de `lines.json`) : valeur de la ligne `full` moins
 * valeur de la ligne `okx_usdc`, nulle quand l'une des deux est indéfinie (docs/specs/s11.md §9.3).
 */
final readonly class ResidualStats
{
    public function __construct(
        public ?float $expectancyR,
        public ?float $expectancyNet,
        public ?float $returnTotal,
        public ?float $maxDrawdown1h,
        public int $nTrades,
    ) {
    }
}
