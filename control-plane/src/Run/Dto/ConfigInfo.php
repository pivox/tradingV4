<?php

declare(strict_types=1);

namespace App\Run\Dto;

/**
 * Configuration hachée du run (`config`), pour l'annexe. En s11, `research` reprend le bloc `config.research`
 * (version des blocs, TF de décision, règle de purge, spécification de l'univers) et les tables par jambe
 * (`portfolio.universe`, `instruments`) sont vides : elles passent dans l'identité de recherche.
 * En s10, `fees` et `slippage_bps` n'existent plus :
 * la grille se lit dans `costs.fee_grid` (`feesExchange` porte alors son nom, par exemple `okx_regular`) et
 * `slippageBps` vaut null (le slippage vient du profil de coûts).
 */
final readonly class ConfigInfo
{
    /**
     * @param array<string, bool|int|float|string|null> $strategyParams
     * @param array<string, bool|int|float|string|null> $simRules
     * @param array<string, bool|int|float|string|null>|null $instrument  règles de l'instrument (s8 seulement)
     * @param array<string, array<string, bool|int|float|string|null>>|null $instruments règles par symbole (s9)
     * @param array<string, bool|int|float|string|null>|null $portfolio   bloc `config.portfolio` (s9)
     * @param array<string, bool|int|float|string|null>|null $research    bloc `config.research` (s11)
     */
    public function __construct(
        public string $strategyId,
        public string $strategyVersion,
        public array $strategyParams,
        public string $feesExchange,
        public float $makerBps,
        public float $takerBps,
        public ?float $slippageBps,
        public float $initialCash,
        public int $minTrades,
        public int $warmupBars,
        public string $tv4Version,
        public array $simRules,
        public ?array $instrument,
        public ?array $instruments = null,
        public ?array $portfolio = null,
        public ?array $research = null,
    ) {
    }
}
