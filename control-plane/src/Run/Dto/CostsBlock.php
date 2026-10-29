<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Bloc `costs` de `metrics.json` (schéma `tv4-metrics-s10`) : profil joué, fichiers de coûts, jambes. */
final readonly class CostsBlock
{
    /**
     * @param list<CostLeg> $legs
     */
    public function __construct(
        public string $profile,
        public bool $gatesProfile,
        public string $rulesVersion,
        public string $feeGrid,
        public int $seed,
        public string $costsFile,
        public string $costsFileHash,
        public ?string $costMeasuresFile,
        public ?string $costMeasuresHash,
        public bool $stressCalibrated,
        public ?string $stressCalibrationRef,
        public array $legs,
    ) {
    }
}
