<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Segment de données chargé par un run portefeuille (`data.series[]`, régime compris). */
final readonly class DataSeries
{
    public function __construct(
        public string $symbol,
        public int $rows,
        public ?int $firstMs,
        public ?int $lastMs,
    ) {
    }
}
