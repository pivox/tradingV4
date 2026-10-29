<?php

declare(strict_types=1);

namespace App\Run\Dto;

use RuntimeException;

/**
 * Une ligne de `lines.json` (`lines[]`) et ce qu'elle désigne : un run s11, ou un dossier de profils de coûts
 * dont les trois runs sont s11.
 */
final readonly class LineEntry
{
    /**
     * @param list<string>                     $legs
     * @param array<string, LineProfileStats>  $byProfile résumé par profil (`single`, ou `baseline`, `adverse`, `maker`)
     */
    public function __construct(
        public string $line,
        public string $dir,
        public string $id,
        public array $legs,
        public array $byProfile,
        public ?RunReport $run,
        public ?ProfilesReport $profiles,
    ) {
    }

    /** Vrai quand la ligne est un dossier de profils de coûts. */
    public function isProfiles(): bool
    {
        return null !== $this->profiles;
    }

    /** Le run de référence de la ligne : le run lui-même, ou le profil baseline d'un dossier de profils. */
    public function referenceRun(): RunReport
    {
        return $this->profiles?->baseline() ?? $this->run ?? throw new RuntimeException('ligne sans run : ' . $this->line);
    }

    public function research(): ResearchInfo
    {
        return $this->referenceRun()->metrics->research ?? throw new RuntimeException('ligne sans bloc research : ' . $this->line);
    }

    public function stats(string $profile): LineProfileStats
    {
        return $this->byProfile[$profile] ?? throw new RuntimeException('profil inconnu : ' . $profile);
    }
}
