<?php

declare(strict_types=1);

namespace App\Run\Dto;

/**
 * Bloc `gates` de `profiles.json` : les éléments des portes croisées de 04 §3.3, jugés sur baseline et adverse.
 * `profiles.json` ne prononce aucun verdict de porte.
 */
final readonly class ProfileGates
{
    public function __construct(
        public ?bool $costRBaseline,
        public ?bool $costRAdverse,
        public bool $costThirdBaseline,
        public bool $costThirdAdverse,
        public ?bool $grossMedianVs2xBaselineCost,
        public bool $bothGateProfilesPositive,
    ) {
    }
}
