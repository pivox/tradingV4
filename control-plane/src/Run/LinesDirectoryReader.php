<?php

declare(strict_types=1);

namespace App\Run;

use App\Run\Dto\LineEntry;
use App\Run\Dto\LineProfileStats;
use App\Run\Dto\LinesReport;
use App\Run\Dto\ResearchInfo;
use App\Run\Dto\ResidualStats;
use App\Run\Json\JsonObject;

/**
 * Lit un dossier de lignes (docs/specs/s11.md §9.3) : `lines.json` (schéma `tv4-lines-v1`) et les deux sous-dossiers
 * `full/` et `okx_usdc/`, chacun un run s11 ou un dossier de profils de coûts dont les trois runs sont s11.
 *
 * Aucune valeur n'est recalculée. Le lecteur recoupe seulement ce que `lines.json` répète des runs (identifiants,
 * bloc, ligne, hashes, effectif, purge, verdict, capital, jambes, sous-ensemble) : un écart veut dire que le fichier
 * ne décrit pas ces sous-dossiers. Les deux lignes partagent le `config_hash` de chaque profil (§9.1).
 */
final class LinesDirectoryReader
{
    public const FILE = 'lines.json';
    public const SCHEMA = 'tv4-lines-v1';

    /** Les deux lignes, dans l'ordre fixe de `lines.json`. @var list<string> */
    public const LINE_NAMES = ['full', 'okx_usdc'];

    /** Clé unique de `by_profile` d'une ligne qui n'est pas un dossier de profils. */
    public const SINGLE = 'single';

    public function __construct(
        private readonly RunDirectoryReader $runs,
        private readonly ProfilesDirectoryReader $profiles,
    ) {
    }

    /** Un dossier qui contient `lines.json` est un dossier de lignes. */
    public static function isLinesDirectory(string $directory): bool
    {
        return is_file(rtrim($directory, '/\\') . '/' . self::FILE);
    }

    /**
     * @throws InvalidRunException
     */
    public function read(string $directory): LinesReport
    {
        if (!is_dir($directory)) {
            throw InvalidRunException::directoryNotFound($directory);
        }
        $dir = rtrim($directory, '/\\');
        $content = is_file($dir . '/' . self::FILE) ? @file_get_contents($dir . '/' . self::FILE) : false;
        if (false === $content) {
            throw InvalidRunException::missingFile(self::FILE);
        }
        $j = JsonObject::decode($content, self::FILE);
        if (!$j->has('schema')) {
            throw InvalidRunException::missingKey(self::FILE, 'schema');
        }
        if (self::SCHEMA !== $j->string('schema')) {
            throw InvalidRunException::unknownSchema(self::FILE, $j->string('schema'), [self::SCHEMA]);
        }

        $block = $j->string('block');
        if (!in_array($block, ResearchInfo::BLOCKS, true)) {
            throw $this->invalid('block', sprintf('valeur inconnue "%s" (attendu : %s)', $block, implode(', ', ResearchInfo::BLOCKS)));
        }
        $configHash = [];
        foreach ($j->scalarMap('config_hash') as $profile => $hash) {
            if (!is_string($hash)) {
                throw InvalidRunException::wrongType(self::FILE, 'config_hash.' . $profile, 'une chaîne', get_debug_type($hash));
            }
            $configHash[$profile] = $hash;
        }

        $entries = $this->entries($j, $dir, array_keys($configHash));
        // `lines.json` est écrit à clés triées : l'ordre d'affichage est celui des profils du moteur.
        $profileNames = array_keys($entries[0]->byProfile);
        $configHash = $this->ordered($configHash, $profileNames);
        $residual = $this->ordered($this->residual($j, $profileNames), $profileNames);

        $report = new LinesReport(
            block: $block,
            datasetHash: $j->string('dataset_hash'),
            universeWindowHash: $j->nullableString('universe_window_hash'),
            profileNames: $profileNames,
            configHash: $configHash,
            subsetKind: $j->string('subset.kind'),
            subsetHash: $j->string('subset.subset_hash'),
            subsetBases: $j->stringList('subset.bases'),
            initialCash: $j->float('initial_cash'),
            lines: $entries,
            residual: $residual,
            linesId: $j->string('lines_id'),
        );
        foreach ($entries as $index => $entry) {
            $this->check($report, $entry, $index);
        }

        return $report;
    }

    /**
     * @param list<string|int> $profileNames clés de `config_hash`
     *
     * @return list<LineEntry>
     */
    private function entries(JsonObject $j, string $dir, array $profileNames): array
    {
        $names = array_map(strval(...), $profileNames);
        $entries = [];
        foreach ($j->objectList('lines') as $index => $o) {
            $at = static fn (string $key): string => sprintf('lines[%d].%s', $index, $key);
            $line = $o->string('line');
            $sub = $o->string('dir');
            if (1 !== preg_match('/^[A-Za-z0-9_-]+$/', $sub)) {
                throw $this->invalid($at('dir'), sprintf('nom de sous-dossier simple attendu, trouvé "%s"', $sub));
            }
            $isProfiles = ProfilesDirectoryReader::isProfilesDirectory($dir . '/' . $sub);
            if ($isProfiles === $o->has('run_id') || $isProfiles !== $o->has('profiles_id')) {
                throw $this->invalid(
                    $at($isProfiles ? 'profiles_id' : 'run_id'),
                    $isProfiles ? 'un dossier de profils se désigne par profiles_id' : 'un run se désigne par run_id',
                );
            }
            $id = $o->string($isProfiles ? 'profiles_id' : 'run_id');
            $expectedProfiles = $isProfiles ? ProfilesDirectoryReader::PROFILE_NAMES : [self::SINGLE];
            if (!self::sameKeys($names, $expectedProfiles)) {
                throw $this->invalid(
                    'config_hash',
                    sprintf(
                        'clés %s attendues pour la ligne %s, trouvé : %s',
                        implode(', ', $expectedProfiles),
                        $line,
                        implode(', ', $names),
                    ),
                );
            }
            $found = $o->keys('by_profile');
            if (!self::sameKeys($found, $expectedProfiles)) {
                throw $this->invalid(
                    $at('by_profile'),
                    sprintf('clés %s attendues, trouvé : %s', implode(', ', $expectedProfiles), implode(', ', $found)),
                );
            }
            $byProfile = [];
            foreach ($expectedProfiles as $profile) {
                $byProfile[$profile] = $this->stats($o->object('by_profile.' . $profile));
            }

            $path = $dir . '/' . $sub;
            try {
                $profiles = $isProfiles ? $this->profiles->read($path) : null;
                $run = $isProfiles ? null : $this->runs->read($path);
            } catch (InvalidRunException $e) {
                throw InvalidRunException::inDirectory($sub, $e);
            }
            $entries[] = new LineEntry($line, $sub, $id, $o->stringList('legs'), $byProfile, $run, $profiles);
        }

        $lineNames = array_map(static fn (LineEntry $e): string => $e->line, $entries);
        if ($lineNames !== self::LINE_NAMES) {
            throw $this->invalid(
                'lines',
                sprintf(
                    'deux lignes %s attendues dans cet ordre, trouvé : %s',
                    implode(', ', self::LINE_NAMES),
                    [] === $lineNames ? 'aucune' : implode(', ', $lineNames),
                ),
            );
        }
        if ($entries[0]->dir === $entries[1]->dir) {
            throw $this->invalid('lines[1].dir', 'les deux lignes ne peuvent pas partager un sous-dossier');
        }
        return $entries;
    }

    private function stats(JsonObject $o): LineProfileStats
    {
        return new LineProfileStats(
            identityHash: $o->string('identity_hash'),
            nTrades: $o->int('n_trades'),
            tradesPurged: $o->int('trades_purged'),
            expectancyR: $o->nullableFloat('expectancy_r'),
            expectancyNet: $o->nullableFloat('expectancy_net'),
            profitFactor: $o->nullableFloat('profit_factor'),
            returnTotal: $o->float('return_total'),
            maxDrawdown1h: $o->float('max_drawdown_1h'),
            verdict: $o->string('verdict'),
        );
    }

    /**
     * @param list<string|int> $profileNames
     *
     * @return array<string, ResidualStats>
     */
    private function residual(JsonObject $j, array $profileNames): array
    {
        $names = array_map(strval(...), $profileNames);
        $found = $j->keys('residual_survivorship');
        if (!self::sameKeys($found, $names)) {
            throw $this->invalid(
                'residual_survivorship',
                sprintf('clés %s attendues, trouvé : %s', implode(', ', $names), implode(', ', $found)),
            );
        }
        $residual = [];
        foreach ($names as $profile) {
            $o = $j->object('residual_survivorship.' . $profile);
            $residual[$profile] = new ResidualStats(
                expectancyR: $o->nullableFloat('expectancy_r'),
                expectancyNet: $o->nullableFloat('expectancy_net'),
                returnTotal: $o->nullableFloat('return_total'),
                maxDrawdown1h: $o->nullableFloat('max_drawdown_1h'),
                nTrades: $o->int('n_trades'),
            );
        }

        return $residual;
    }

    /**
     * Recoupe `lines.json` avec les runs de la ligne : ce que le fichier répète doit être ce que les runs ont écrit.
     */
    private function check(LinesReport $report, LineEntry $entry, int $index): void
    {
        $at = static fn (string $key): string => sprintf('lines[%d].%s', $index, $key);
        $runs = null !== $entry->profiles ? $entry->profiles->runs : (null !== $entry->run ? [self::SINGLE => $entry->run] : []);
        $declared = null !== $entry->profiles ? $entry->profiles->profilesId : $entry->run?->manifest->runId;
        if ($entry->id !== $declared) {
            throw $this->invalid($at($entry->isProfiles() ? 'profiles_id' : 'run_id'), 'différent de celui du dossier ' . $entry->dir);
        }

        foreach ($runs as $profile => $run) {
            $where = $entry->dir . ($entry->isProfiles() ? '/' . $profile : '');
            $research = $run->metrics->research;
            if (null === $research) {
                throw $this->invalid($at('dir'), sprintf('"%s" n\'est pas un run s11', $where));
            }
            $stats = $entry->stats($profile);
            // Chaque contrôle : chemin dans lines.json, valeur du fichier, valeur du run, et ce que le run en dit.
            $stat = fn (string $key): string => $at('by_profile.' . $profile . '.' . $key);
            $manifest = $run->manifest;
            $checks = [
                ['block', $report->block, $research->block, 'research.block du run'],
                [$at('line'), $entry->line, $research->line, 'research.line du run'],
                ['dataset_hash', $report->datasetHash, $manifest->datasetHash, 'dataset_hash du manifeste de'],
                [
                    'universe_window_hash',
                    $report->universeWindowHash,
                    $research->universe->windowHash,
                    'research.universe.window_hash du run',
                ],
                [
                    'config_hash.' . $profile,
                    $report->configHash[$profile] ?? null,
                    $manifest->configHash,
                    'config_hash du manifeste de',
                ],
                ['initial_cash', $report->initialCash, $manifest->config->initialCash, 'initial_cash du run'],
                [$at('legs'), $entry->legs, $manifest->data->symbols, 'data.symbols du manifeste de'],
                [$stat('identity_hash'), $stats->identityHash, $research->identityHash, 'research.identity_hash du run'],
                [$stat('n_trades'), $stats->nTrades, $run->metrics->nTrades, 'n_trades du run'],
                [$stat('trades_purged'), $stats->tradesPurged, $research->purge->tradesPurged, 'research.purge.trades_purged du run'],
                [$stat('verdict'), $stats->verdict, $run->metrics->verdict, 'verdict du run'],
            ];
            foreach ($checks as [$path, $found, $expected, $what]) {
                if ($found !== $expected) {
                    throw $this->invalid($path, sprintf('différent de %s %s', $what, $where));
                }
            }
            if ('okx_usdc' === $entry->line) {
                $subset = $research->subset;
                if (null === $subset || $subset->kind !== $report->subsetKind || $subset->subsetHash !== $report->subsetHash) {
                    throw $this->invalid('subset', 'différent de research.subset du run ' . $where);
                }
            }
        }
    }

    /**
     * Mêmes clés, dans n'importe quel ordre.
     *
     * @param list<string|int> $found
     * @param list<string|int> $expected
     */
    private static function sameKeys(array $found, array $expected): bool
    {
        $found = array_map(strval(...), $found);
        $expected = array_map(strval(...), $expected);
        sort($found);
        sort($expected);

        return $found === $expected;
    }

    /**
     * @template T
     *
     * @param array<array-key, T> $map
     * @param list<string|int>    $order
     *
     * @return array<string, T>
     */
    private function ordered(array $map, array $order): array
    {
        $out = [];
        foreach ($order as $key) {
            $out[(string) $key] = $map[$key];
        }

        return $out;
    }

    private function invalid(string $path, string $reason): InvalidRunException
    {
        return InvalidRunException::invalidValue(self::FILE, $path, $reason);
    }
}
