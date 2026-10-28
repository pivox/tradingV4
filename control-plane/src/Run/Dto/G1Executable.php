<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Bloc `metrics.g1_executable` (schéma s9) : part des entrées exécutables, motifs distingués. */
final readonly class G1Executable
{
    /**
     * @param array<string, int> $counts compteurs publiés (`fills`, `rejected_volume`, `rejected_min_notional`,
     *                                   `rejected_lot_step`, `rejected_cash`, `rejected_max_positions`)
     */
    public function __construct(
        public ?float $ratio,
        public ?float $threshold,
        public string $verdict,
        public bool $atReferenceCapital,
        public array $counts,
    ) {
    }
}
