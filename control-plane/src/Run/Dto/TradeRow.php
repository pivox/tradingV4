<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Une ligne de `trades.csv` (36 colonnes). */
final readonly class TradeRow
{
    public function __construct(
        public int $lotId,
        public int $entrySignalMs,
        public int $entryMs,
        public string $entryLiquidity,
        public float $entryRefPrice,
        public float $entryPrice,
        public float $entryFee,
        public float $entrySlippage,
        public int $exitSignalMs,
        public int $exitMs,
        public string $exitReason,
        public string $exitLiquidity,
        public float $exitRefPrice,
        public float $exitPrice,
        public float $exitFee,
        public float $exitSlippage,
        public float $qty,
        public float $pnlNet,
        public float $mfe,
        public float $mae,
        public ?float $stopAtExit,
        public bool $cappedByCash,
        public bool $forcedClose,
        public string $entryTag,
        public string $exitTag,
        public string $symbol,
        public int $entryOrderMs,
        public int $exitOrderMs,
        public string $exitType,
        public int $holdingMs,
        public float $positionTimeShare,
        public ?float $stopAtEntry,
        public ?float $riskQuote,
        public ?float $rMultiple,
        public ?string $regimeAtEntry,
        public ?float $spreadCost,
    ) {
    }
}
