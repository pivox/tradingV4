<?php

declare(strict_types=1);

namespace App\Run\Dto;

use RuntimeException;

/**
 * Un dossier de profils lu : `profiles.json` (schéma `tv4-profiles-v1`) et les trois runs s10 qu'il désigne.
 * Le control plane ne recalcule ni étiquette ni statut : il affiche ce que `tv4` a écrit.
 */
final readonly class ProfilesReport
{
    /**
     * @param list<ProfileSummary>  $profiles dans l'ordre du fichier
     * @param array<string, RunReport> $runs  runs lus, par nom de profil
     */
    public function __construct(
        public string $profilesId,
        public string $datasetHash,
        public float $initialCash,
        public int $seed,
        public string $feeGrid,
        public string $costsFileHash,
        public ?string $costMeasuresHash,
        public bool $stressCalibrated,
        public array $profiles,
        public MakerStatus $maker,
        public ProfileGates $gates,
        public string $label,
        public string $labelFr,
        public array $runs,
    ) {
    }

    /** La ligne de `profiles[]` du profil. */
    public function profile(string $name): ProfileSummary
    {
        foreach ($this->profiles as $summary) {
            if ($summary->name === $name) {
                return $summary;
            }
        }

        throw new RuntimeException('profil inconnu : ' . $name);
    }

    public function run(string $name): RunReport
    {
        return $this->runs[$name] ?? throw new RuntimeException('profil inconnu : ' . $name);
    }

    /** Le run de référence du rapport : la fiche complète du profil baseline. */
    public function baseline(): RunReport
    {
        return $this->run('baseline');
    }

    /** Seuil coût/stop (en R) du profil, tel que son run l'a écrit. */
    public function costRLimit(string $name): ?float
    {
        return $this->run($name)->metrics->costArithmetic?->costRLimit;
    }
}
