<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Bloc `metrics.instruments` (schéma s9) : origine des règles d'instrument (`okx` ou `toml`). */
final readonly class InstrumentsInfo
{
    /**
     * @param list<string>         $notLive         instruments dont l'état n'est pas `live`
     * @param list<UpcomingChange> $upcomingChanges
     */
    public function __construct(
        public string $source,
        public ?string $file,
        public ?string $instrumentsHash,
        public ?string $fetchedAt,
        public array $notLive,
        public array $upcomingChanges,
    ) {
    }
}
