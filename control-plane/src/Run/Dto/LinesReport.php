<?php

declare(strict_types=1);

namespace App\Run\Dto;

use RuntimeException;

/**
 * Un dossier de lignes lu : `lines.json` (schéma `tv4-lines-v1`, docs/specs/s11.md §9.3) et les deux lignes qu'il
 * désigne, `full` et `okx_usdc`. Le control plane ne recalcule rien : la survivance résiduelle est celle que `tv4` a
 * écrite.
 */
final readonly class LinesReport
{
    /**
     * @param list<string>                    $profileNames   clés de `by_profile` : `single`, ou baseline, adverse, maker
     * @param array<string, string>           $configHash     `config_hash` commun aux deux lignes, par profil
     * @param list<string>                    $subsetBases
     * @param list<LineEntry>                 $lines
     * @param array<string, ResidualStats>    $residual       survivance résiduelle, par profil
     */
    public function __construct(
        public string $block,
        public string $datasetHash,
        public ?string $universeWindowHash,
        public array $profileNames,
        public array $configHash,
        public string $subsetKind,
        public string $subsetHash,
        public array $subsetBases,
        public float $initialCash,
        public array $lines,
        public array $residual,
        public string $linesId,
    ) {
    }

    public function line(string $name): LineEntry
    {
        foreach ($this->lines as $entry) {
            if ($entry->line === $name) {
                return $entry;
            }
        }

        throw new RuntimeException('ligne inconnue : ' . $name);
    }

    /** La ligne `full`, dont la fiche complète est rendue ; la fiche de `okx_usdc` se rend depuis son sous-dossier. */
    public function full(): LineEntry
    {
        return $this->line('full');
    }

    /** Vrai quand les lignes sont des dossiers de profils de coûts. */
    public function isProfiles(): bool
    {
        return $this->full()->isProfiles();
    }

    /** Survivance résiduelle d'un profil. */
    public function residualOf(string $profile): ResidualStats
    {
        return $this->residual[$profile] ?? throw new RuntimeException('profil inconnu : ' . $profile);
    }
}
