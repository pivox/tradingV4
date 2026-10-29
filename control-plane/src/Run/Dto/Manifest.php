<?php

declare(strict_types=1);

namespace App\Run\Dto;

/**
 * Contenu de `manifest.json` (schémas `tv4-manifest-s8`, `tv4-manifest-s9`, `tv4-manifest-s10` et `tv4-manifest-s11`).
 * Les champs `profile`, `costs_file*`, `cost_measures*` et `profiles_id` n'existent qu'en s10 (et en s11 joué avec
 * un profil de coûts) et peuvent y manquer. `research_inputs` (chemins du dossier d'univers et du sous-ensemble OKX,
 * tels que donnés à `tv4 backtest`) n'existe qu'en s11.
 */
final readonly class Manifest
{
    /**
     * @param array<string, string> $fileHashes hashes `blake3:<hex>` des fichiers du run
     * @param array<string, bool|int|float|string|null>|null $researchInputs chemins d'entrée de la recherche (s11)
     */
    public function __construct(
        public string $runId,
        public string $configHash,
        public string $datasetHash,
        public string $hashFormat,
        public string $resultHash,
        public string $verdict,
        public ConfigInfo $config,
        public ManifestData $data,
        public RunWindow $window,
        public EngineInfo $engine,
        public string $equityStep,
        public array $fileHashes,
        public string $startedAt,
        public Timings $timings,
        public string $schema = 'tv4-manifest-s8',
        public ?string $instrumentsHash = null,
        public ?string $instrumentsFile = null,
        public ?string $profile = null,
        public ?string $costsFile = null,
        public ?string $costsFileHash = null,
        public ?string $costMeasuresFile = null,
        public ?string $costMeasuresHash = null,
        public ?string $profilesId = null,
        public ?array $researchInputs = null,
    ) {
    }
}
