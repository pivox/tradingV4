<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Configuration hachée du run (`config`), pour l'annexe. */
final readonly class ConfigInfo
{
    /**
     * @param array<string, bool|int|float|string|null> $strategyParams
     * @param array<string, bool|int|float|string|null> $simRules
     * @param array<string, bool|int|float|string|null> $instrument
     */
    public function __construct(
        public string $strategyId,
        public string $strategyVersion,
        public array $strategyParams,
        public string $feesExchange,
        public float $makerBps,
        public float $takerBps,
        public float $slippageBps,
        public float $initialCash,
        public int $minTrades,
        public int $warmupBars,
        public string $tv4Version,
        public array $simRules,
        public array $instrument,
    ) {
    }
}
