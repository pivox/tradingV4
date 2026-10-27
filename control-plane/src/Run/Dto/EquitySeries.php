<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Courbe d'equity échantillonnée : trois listes parallèles de scalaires, pas un objet par point. */
final readonly class EquitySeries
{
    /**
     * @param list<int>   $timesMs
     * @param list<float> $equity
     * @param list<float> $drawdown fraction négative ou nulle
     */
    public function __construct(
        public array $timesMs,
        public array $equity,
        public array $drawdown,
    ) {
    }

    public function count(): int
    {
        return count($this->timesMs);
    }
}
