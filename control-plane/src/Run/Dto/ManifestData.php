<?php

declare(strict_types=1);

namespace App\Run\Dto;

/**
 * Données rejouées telles que le manifeste les décrit (`manifest.data`).
 *
 * Un run s8 porte une paire (`symbol`) et les champs `source`, `bars`, `synthetic_bars`,
 * `first_open_time_ms` et `last_open_time_ms` ; un run s9 porte l'univers (`symbols`), la paire de
 * régime et les segments chargés (`series`), et ces champs peuvent manquer (null). Un run s11 porte en plus le bloc de
 * recherche du dataset (`block`) et sa fenêtre de décision (`decision_window`).
 */
final readonly class ManifestData
{
    /**
     * @param list<string>     $symbols univers : une paire en s8, l'univers de la config en s9
     * @param list<DataSeries> $series  segments chargés (régime compris), vide en s8
     */
    public function __construct(
        public ?string $source,
        public ?string $dataset,
        public ?string $fixtureBlake3,
        public string $exchange,
        public array $symbols,
        public string $tf,
        public string $gapsPolicy,
        public ?int $bars,
        public ?int $syntheticBars,
        public ?int $firstOpenTimeMs,
        public ?int $lastOpenTimeMs,
        public ?string $regimeSymbol = null,
        public array $series = [],
        public ?string $block = null,
        public ?RunWindow $decisionWindow = null,
    ) {
    }

    /** Univers lisible : les symboles séparés par une virgule. */
    public function symbolsLabel(): string
    {
        return implode(', ', $this->symbols);
    }
}
