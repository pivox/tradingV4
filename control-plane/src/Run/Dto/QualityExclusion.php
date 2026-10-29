<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Une exclusion qualité d'une paire (`research.quality_exclusions`) : un trimestre et les TF exclus. */
final readonly class QualityExclusion
{
    /**
     * @param list<string> $tfs
     */
    public function __construct(
        public string $quarter,
        public array $tfs,
    ) {
    }
}
