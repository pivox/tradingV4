<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Bloc `maker` de `profiles.json` : statut du profil maker, séparé de l'étiquette (04 §3 : « informatif »). */
final readonly class MakerStatus
{
    public function __construct(
        public string $status,
        public int $nTrades,
        public ?float $expectancyNet,
        public int $entriesConverted,
        public int $entriesTakenAtArrival,
        public int $entriesFilledMaker,
        public ?float $makerEntryMakerShare,
        public float $minMakerEntryShare,
    ) {
    }
}
