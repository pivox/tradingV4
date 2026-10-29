<?php

declare(strict_types=1);

namespace App\Run\Dto;

/**
 * Résumé d'une ligne pour un profil (`lines[].by_profile.<profil>` de `lines.json`) : ce que `tv4` a écrit,
 * recoupé par le lecteur avec le run correspondant (identité, effectif, purge, verdict).
 */
final readonly class LineProfileStats
{
    public function __construct(
        public string $identityHash,
        public int $nTrades,
        public int $tradesPurged,
        public ?float $expectancyR,
        public ?float $expectancyNet,
        public ?float $profitFactor,
        public float $returnTotal,
        public float $maxDrawdown1h,
        public string $verdict,
    ) {
    }
}
