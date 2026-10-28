<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Contenu de `manifest.json` (schémas `tv4-manifest-s8` et `tv4-manifest-s9`). */
final readonly class Manifest
{
    /**
     * @param array<string, string> $fileHashes hashes `blake3:<hex>` des fichiers du run
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
    ) {
    }
}
