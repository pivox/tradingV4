<?php

declare(strict_types=1);

namespace App\Run\Dto;

/**
 * Bloc `research.identity` (schéma s11, docs/specs/s11.md §8.6) : ce qui distingue deux runs de même `config_hash` sur
 * le même `dataset_hash`. Son hash est `research.identity_hash`.
 */
final readonly class ResearchIdentity
{
    public function __construct(
        public string $block,
        public int $decisionStartMs,
        public int $decisionEndMs,
        public ?string $universeWindowHash,
        public string $qualityHash,
        public string $line,
        public ?string $subsetHash,
        public string $legsHash,
    ) {
    }
}
