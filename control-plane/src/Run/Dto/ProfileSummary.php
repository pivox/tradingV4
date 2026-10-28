<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Une ligne de `profiles.json` (`profiles[]`, schéma `tv4-profiles-v1`) : le résumé d'un profil de coûts. */
final readonly class ProfileSummary
{
    public function __construct(
        public string $name,
        public string $dir,
        public string $role,
        public string $runId,
        public string $configHash,
        public string $resultHash,
        public string $verdict,
        public int $nTrades,
        public ?float $expectancyNet,
        public ?float $expectancyR,
        public float $pnlNet,
        public ?float $roundTripCostRateMean,
        public ?float $costRMean,
        public ?bool $costRPass,
        public bool $costThirdPass,
        public ?float $grossMedianRate,
        public bool $positive,
    ) {
    }

    /** Vrai pour un profil jugé par les portes (baseline, adverse), faux pour le profil informatif (maker). */
    public function isGate(): bool
    {
        return 'gate' === $this->role;
    }
}
