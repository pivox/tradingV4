<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Moteur qui a produit le run (`manifest.engine`), affiché tel que `tv4` l'a écrit. */
final readonly class EngineInfo
{
    public function __construct(
        public string $tv4Version,
        public string $gitSha,
        public string $rustc,
        public string $rustcCommit,
        public string $target,
        public string $targetCpu,
        public string $profile,
        public string $binaryHash,
    ) {
    }
}
