<?php

declare(strict_types=1);

namespace App\Run\Dto;

/**
 * Bloc `research` de `metrics.json` (schéma `tv4-metrics-s11`, docs/specs/s11.md §9.2) : bloc de recherche joué, fenêtre
 * de décision, univers, ligne, identité de recherche, purge et refus d'entrée. Obligatoire en s11, absent avant.
 *
 * Les métriques de trades du run portent sur les trades conservés (`metricsBasis = kept_trades`) ; les métriques
 * d'equity portent sur la fenêtre de décision, PnL purgé compris.
 */
final readonly class ResearchInfo
{
    /** @var list<string> */
    public const BLOCKS = ['DEV', 'VALIDATION', 'HOLDOUT', 'FORWARD'];

    /** @var list<string> */
    public const LINES = ['full', 'okx_usdc'];

    /** @var list<string> */
    public const GUARDS = ['none', 'unguarded'];

    /**
     * @param array<string, list<QualityExclusion>> $qualityExclusions exclusions par symbole
     * @param list<ResearchLeg>                      $legs
     */
    public function __construct(
        public string $block,
        public string $blocksVersion,
        public string $guard,
        public RunWindow $decisionWindow,
        public string $warmupStart,
        public ResearchUniverse $universe,
        public string $line,
        public ?ResearchSubset $subset,
        public bool $regimeUsed,
        public ResearchIdentity $identity,
        public string $identityHash,
        public array $qualityExclusions,
        public ResearchPurge $purge,
        public string $metricsBasis,
        public ResearchFunnel $funnel,
        public array $legs,
    ) {
    }

    /** Vrai pour un run joué avec l'option cachée `--unguarded-protected-read` (docs/specs/s11.md §9.5). */
    public function isUnguarded(): bool
    {
        return 'unguarded' === $this->guard;
    }
}
