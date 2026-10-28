<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Bloc `metrics.executability` (schéma s9) : règle G0 de la liste live, publiée par `tv4`. */
final readonly class Executability
{
    /**
     * @param list<ExecutabilityPair>      $pairs
     * @param list<string>                 $liveList
     * @param list<ExecutabilityRejection> $rejected
     */
    public function __construct(
        public float $referenceCapital,
        public string $rule,
        public bool $venueMatchesExecution,
        public array $pairs,
        public array $liveList,
        public array $rejected,
    ) {
    }
}
