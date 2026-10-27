<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Fenêtre jouée, après warmup (`manifest.window`). `end` est exclue. */
final readonly class RunWindow
{
    public function __construct(
        public string $start,
        public string $end,
        public int $startMs,
        public int $endMs,
    ) {
    }
}
