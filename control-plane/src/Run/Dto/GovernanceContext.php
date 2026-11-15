<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Provenance déclarée par le moteur, sans recalcul côté présentation. */
final readonly class GovernanceContext
{
    public function __construct(
        public string $schema,
        public string $configHash,
        public string $datasetHash,
        public int $trialId,
        public int $readId,
        public ?HypothesisContext $hypothesis,
        public ?string $frozenTag,
        public ?string $frozenAt,
        public ?string $sourceBlock,
        public ?string $effectiveBlock,
        public int $generation,
        public ?int $claimHoldout,
        public NoiseContext $noise,
    ) {
    }
}
