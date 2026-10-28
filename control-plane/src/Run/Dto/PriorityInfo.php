<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Bloc `metrics.priority` (schéma s9) : règle de priorité des signaux et origine du rang de liquidité. */
final readonly class PriorityInfo
{
    public function __construct(
        public string $rule,
        public int $liquidityWindowDays,
        public string $rankSource,
        public ?string $rankExchange,
        public int $rankDays,
        public int $partialDays,
    ) {
    }
}
