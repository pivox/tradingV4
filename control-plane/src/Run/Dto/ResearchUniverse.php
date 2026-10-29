<?php

declare(strict_types=1);

namespace App\Run\Dto;

/**
 * Bloc `research.universe` (schéma s11). Le mode `quarterly` porte `rules_hash`, `window_hash`, `decision_tf` et les
 * trimestres lus ; le mode `fixed` (jambes données par `--symbols`) peut les laisser nuls ou absents.
 */
final readonly class ResearchUniverse
{
    public const MODE_QUARTERLY = 'quarterly';
    public const MODE_FIXED = 'fixed';

    /** @var list<string> */
    public const MODES = [self::MODE_QUARTERLY, self::MODE_FIXED];

    /**
     * @param list<ResearchQuarter> $quarters
     */
    public function __construct(
        public string $mode,
        public ?string $rulesHash,
        public ?string $windowHash,
        public ?string $decisionTf,
        public array $quarters,
    ) {
    }

    public function isQuarterly(): bool
    {
        return self::MODE_QUARTERLY === $this->mode;
    }
}
