<?php

declare(strict_types=1);

namespace App\Tests\Support;

use App\Run\Json\JsonObject;

/** Contexte synthétique ajouté en copie temporaire ; aucune métrique de simulation n'est modifiée. */
final class GovernanceFixture
{
    /** @return array<string, mixed> */
    public static function context(RunFixture $run, string $file = 'manifest.json', ?string $block = null): array
    {
        $manifest = JsonObject::decode($run->read($file), $file);

        return [
            'schema' => 'tv4-governance-v1',
            'config_hash' => $manifest->string('config_hash'),
            'dataset_hash' => $manifest->string('dataset_hash'),
            'trial_id' => 17,
            'read_id' => 23,
            'hypothesis' => ['identifiant' => 'H-001', 'sha256' => str_repeat('a', 64)],
            'frozen_tag' => 'frozen/H-001',
            'frozen_at' => '2026-01-02T03:04:05Z',
            'source_block' => $block,
            'effective_block' => $block,
            'generation' => 0,
            'claim_holdout' => 'HOLDOUT' === $block ? 3 : null,
            'noise' => ['n' => 50, 'n_eff' => null, 'selection_years' => 5.0,
                'expected' => 1.01799, 'approx_q90' => 1.25092, 'q95' => 1.37867],
        ];
    }
}
