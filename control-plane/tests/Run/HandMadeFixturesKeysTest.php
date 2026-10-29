<?php

declare(strict_types=1);

namespace App\Tests\Run;

use App\Tests\Support\RunFixture;
use PHPUnit\Framework\TestCase;
use RuntimeException;

/**
 * Les fixtures s11 écrites à la main (`run-recherche-trimestriel/`, `run-lignes-garde/`, `run-lignes-profils/`) ne
 * doivent porter aucune clé que `tv4` n'écrit pas : chaque chemin de clés de leurs `metrics.json`, `manifest.json` et
 * `lines.json` doit exister dans une sortie réelle, soit les fixtures `run-recherche/` et `run-lignes/` (CLI sur la
 * fixture Kraken), soit les goldens Rust (`research_lines`, `portfolio_profiles`).
 *
 * Le type des feuilles est ignoré (un nombre ou `null` selon les données), de même que les noms de paires et les années
 * (clés de dictionnaires de données). Les valeurs ne sont pas comparées : elles sont fabriquées, dites telles dans les
 * `README.md` des fixtures.
 */
final class HandMadeFixturesKeysTest extends TestCase
{
    private const GOLDENS = __DIR__ . '/../../../tv4/tests/golden_runs';

    /**
     * Seules clés admises sans équivalent dans les sorties réelles versionnées, toutes vérifiées sur une sortie réelle
     * non versionnée (univers trimestriel construit sur un zip synthétique, avec et sans `--costs`) : `config.costs.legs`
     * est vidé en recherche (docs/specs/s11.md §8.6) et aucune sortie versionnée ne combine recherche et profils de
     * coûts ; la spécification d'univers d'un `config_hash` trimestriel porte `rules_hash` ; une ligne qui est un dossier
     * de profils se désigne par `profiles_id`. Les clés de manifeste d'un profil de coûts viennent de s10, dont le
     * manifeste réel n'est pas versionné.
     *
     * @var list<string>
     */
    private const ALLOWED = [
        '/config/costs/legs{}',
        '/config/research/universe_spec/rules_hash',
        '/lines[]/profiles_id',
        '/costs_file',
        '/costs_file_hash',
        '/profile',
        '/profiles_id',
    ];

    /**
     * @return array<string, true>
     */
    private static function paths(mixed $value, string $prefix = ''): array
    {
        if ($value instanceof \stdClass) {
            $fields = get_object_vars($value);
            if ([] === $fields) {
                return [$prefix . '{}' => true];
            }
            $out = [];
            foreach ($fields as $key => $child) {
                $out += self::paths($child, $prefix . '/' . self::normalise((string) $key));
            }

            return $out;
        }
        if (is_array($value)) {
            if ([] === $value) {
                return [$prefix . '[]' => true];
            }
            $out = [];
            foreach ($value as $child) {
                $out += self::paths($child, $prefix . '[]');
            }

            return $out;
        }

        return [$prefix => true];
    }

    /** Un nom de paire ou une année est une clé de données, pas une clé de schéma. */
    private static function normalise(string $key): string
    {
        if (1 === preg_match('/^[A-Z0-9]+-USD$/', $key)) {
            return '<SYM>';
        }
        if (1 === preg_match('/^(19|20)\d\d$/', $key)) {
            return '<YEAR>';
        }

        return $key;
    }

    /**
     * @return array<string, true>
     */
    private static function keysOf(string $file): array
    {
        $json = file_get_contents($file);
        if (false === $json) {
            throw new RuntimeException('fichier illisible : ' . $file);
        }

        return self::paths(json_decode($json, false, 512, JSON_THROW_ON_ERROR));
    }

    /**
     * @param list<string> $files
     *
     * @return array<string, true>
     */
    private static function union(array $files): array
    {
        $keys = [];
        foreach ($files as $file) {
            $keys += self::keysOf($file);
        }

        return $keys;
    }

    /**
     * @return list<string>
     */
    private static function glob(string $pattern): array
    {
        $files = glob($pattern);

        return false === $files ? [] : $files;
    }

    /**
     * @param array<string, true> $real
     *
     * @return list<string>
     */
    private static function unknownKeys(string $file, array $real): array
    {
        $unknown = array_diff(array_keys(self::keysOf($file)), array_keys($real), self::ALLOWED);
        sort($unknown);

        return $unknown;
    }

    public function testTheHandMadeMetricsOnlyUseKeysThatTv4Writes(): void
    {
        $real = self::union([
            ...self::glob(RunFixture::RESEARCH_SOURCE . '/metrics.json'),
            ...self::glob(RunFixture::LINES_SOURCE . '/*/metrics.json'),
            ...self::glob(self::GOLDENS . '/research_lines/*/metrics.json'),
            ...self::glob(self::GOLDENS . '/portfolio_profiles/*/metrics.json'),
        ]);
        self::assertNotSame([], $real);

        $files = [
            ...self::glob(RunFixture::RESEARCH_QUARTERLY_SOURCE . '/metrics.json'),
            ...self::glob(RunFixture::LINES_GUARD_SOURCE . '/*/metrics.json'),
            ...self::glob(RunFixture::LINES_PROFILES_SOURCE . '/*/*/metrics.json'),
        ];
        self::assertCount(1 + 2 + 6, $files);
        foreach ($files as $file) {
            self::assertSame([], self::unknownKeys($file, $real), $file);
        }
    }

    public function testTheHandMadeManifestsOnlyUseKeysThatTv4Writes(): void
    {
        $real = self::union([
            ...self::glob(RunFixture::RESEARCH_SOURCE . '/manifest.json'),
            ...self::glob(RunFixture::LINES_SOURCE . '/*/manifest.json'),
        ]);
        self::assertNotSame([], $real);
        // `config` du manifeste est la configuration de `metrics.json` : les goldens, qui n'ont pas de manifeste, la fournissent.
        foreach (
            self::union([
                ...self::glob(self::GOLDENS . '/research_lines/*/metrics.json'),
                ...self::glob(self::GOLDENS . '/portfolio_profiles/*/metrics.json'),
            ]) as $path => $_
        ) {
            if (str_starts_with($path, '/config/')) {
                $real[$path] = true;
            }
        }
        // `research_inputs` d'un run à univers trimestriel porte le chemin du dossier d'univers.
        $real += ['/research_inputs/universe' => true, '/research_inputs/okx_subset' => true];

        $files = [
            ...self::glob(RunFixture::RESEARCH_QUARTERLY_SOURCE . '/manifest.json'),
            ...self::glob(RunFixture::LINES_GUARD_SOURCE . '/*/manifest.json'),
            ...self::glob(RunFixture::LINES_PROFILES_SOURCE . '/*/*/manifest.json'),
        ];
        self::assertCount(1 + 2 + 6, $files);
        foreach ($files as $file) {
            self::assertSame([], self::unknownKeys($file, $real), $file);
        }
    }

    public function testTheHandMadeLinesFilesOnlyUseKeysThatTv4Writes(): void
    {
        $real = self::union([
            ...self::glob(RunFixture::LINES_SOURCE . '/lines.json'),
            ...self::glob(self::GOLDENS . '/research_lines/lines.json'),
        ]);
        self::assertNotSame([], $real);

        $files = [
            ...self::glob(RunFixture::LINES_GUARD_SOURCE . '/lines.json'),
            ...self::glob(RunFixture::LINES_PROFILES_SOURCE . '/lines.json'),
        ];
        self::assertCount(2, $files);
        // Un dossier de lignes de profils écrit trois clés par profil (`baseline`, `adverse`, `maker`) au lieu de `single`.
        $profileKeys = [];
        foreach (array_keys($real) as $path) {
            foreach (['baseline', 'adverse', 'maker'] as $profile) {
                $profileKeys[str_replace('/single', '/' . $profile, $path)] = true;
            }
        }
        foreach ($files as $file) {
            self::assertSame([], self::unknownKeys($file, $real + $profileKeys), $file);
        }
    }

    public function testTheHandMadeProfilesFilesOnlyUseKeysThatTv4Writes(): void
    {
        $real = self::union(self::glob(self::GOLDENS . '/portfolio_profiles/profiles.json'));
        self::assertNotSame([], $real);

        foreach (self::glob(RunFixture::LINES_PROFILES_SOURCE . '/*/profiles.json') as $file) {
            self::assertSame([], self::unknownKeys($file, $real), $file);
        }
    }
}
