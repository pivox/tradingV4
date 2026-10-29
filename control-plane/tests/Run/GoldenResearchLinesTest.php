<?php

declare(strict_types=1);

namespace App\Tests\Run;

use App\Run\Csv\EquityCsvReader;
use App\Run\Csv\TradesCsvReader;
use App\Run\Dto\LinesReport;
use App\Run\LinesDirectoryReader;
use App\Run\ProfilesDirectoryReader;
use App\Run\RunDirectoryReader;
use App\Tests\Support\RunFixture;
use RuntimeException;
use Symfony\Bundle\FrameworkBundle\Console\Application;
use Symfony\Bundle\FrameworkBundle\Test\KernelTestCase;
use Symfony\Component\Console\Tester\CommandTester;

/**
 * Lit la vraie sortie de `tv4 backtest` sur un dataset de bloc avec `--okx-subset` : le golden
 * `tv4/tests/golden_runs/research_lines/` de docs/specs/s11.md §13 (`full/`, `okx_usdc/` et `lines.json`).
 *
 * Le golden n'a pas de manifeste (comme `portfolio_2legs` et `portfolio_profiles`, il ne contient que des fichiers
 * déterministes) : le test en écrit un par run dans un dossier temporaire, de la forme du manifeste réel (schéma
 * `tv4-manifest-s11`), dont les identifiants sont ceux de `lines.json` (ou de `profiles.json`) parce que le lecteur les
 * recoupe. Un run qui porte déjà son `manifest.json` le garde. Chaque ligne est un run ou, avec `--costs`, un dossier
 * de profils.
 *
 * Ce test garde les clés que suppose le lecteur : un bloc renommé côté Rust, puis un golden régénéré, le ferait
 * échouer. Aucune valeur numérique du golden n'y est épinglée, il peut être régénéré. Il reste marqué « skipped » tant
 * que le golden du lot E n'existe pas ; le pilote retire le saut à l'intégration (les fixtures `run-recherche/` et
 * `run-lignes/` sont alors remplacées par cette sortie).
 */
final class GoldenResearchLinesTest extends KernelTestCase
{
    private const GOLDEN = __DIR__ . '/../../../tv4/tests/golden_runs/research_lines';

    private ?RunFixture $run = null;

    protected function setUp(): void
    {
        parent::setUp();
        if (!is_dir(self::GOLDEN)) {
            self::markTestSkipped(
                'golden Rust absent : tv4/tests/golden_runs/research_lines/ (full/, okx_usdc/ et lines.json, lot E de docs/specs/s11.md)',
            );
        }
    }

    protected function tearDown(): void
    {
        $this->run?->cleanup();
        $this->run = null;
        parent::tearDown();
    }

    /**
     * @return array<array-key, mixed>
     */
    private static function object(mixed $value, string $what): array
    {
        if (!is_array($value)) {
            throw new RuntimeException('objet JSON attendu : ' . $what);
        }

        return $value;
    }

    /**
     * @return array<array-key, mixed>
     */
    private static function json(string $path): array
    {
        $json = file_get_contents($path);
        if (false === $json) {
            throw new RuntimeException('golden illisible : ' . $path);
        }

        return self::object(json_decode($json, true, 512, JSON_THROW_ON_ERROR), $path);
    }

    /**
     * Manifeste s11 de la forme que tv4 écrit, d'après les métriques du run.
     *
     * @return array<string, mixed>
     */
    private static function manifest(string $metricsPath, string $runId, string $resultHash, ?string $profilesId): array
    {
        $zero = 'blake3:' . str_repeat('0', 64);
        $metrics = self::json($metricsPath);
        $data = self::object($metrics['data'] ?? null, 'data');
        $config = self::object($metrics['config'] ?? null, 'config');
        $research = self::object($metrics['research'] ?? null, 'research');
        $data['block'] = $research['block'] ?? null;
        $data['decision_window'] = $research['decision_window'] ?? null;
        $manifest = [
            'schema' => 'tv4-manifest-s11',
            'run_id' => $runId,
            'config' => $config,
            'config_hash' => $metrics['config_hash'] ?? null,
            'data' => $data,
            'dataset_hash' => $metrics['dataset_hash'] ?? null,
            'hash_format' => $data['hash_format'] ?? null,
            'initial_cash' => $config['initial_cash'] ?? null,
            'engine' => [
                'binary_hash' => $zero,
                'git_sha' => '000000000000',
                'profile' => 'debug',
                'rustc' => '1.97.0',
                'rustc_commit' => str_repeat('0', 40),
                'target' => 'x86_64-unknown-linux-gnu',
                'target_cpu' => 'x86-64-v3',
                'tv4_version' => '0.1.0',
            ],
            'outputs' => [
                'equity_step' => '1h',
                'files' => ['equity.csv' => $zero, 'metrics.json' => $zero, 'trades.csv' => $zero],
            ],
            'research_inputs' => ['universe' => 'golden'],
            'result_hash' => $resultHash,
            'started_at' => '2026-10-29T00:00:00Z',
            'timings' => ['cli_cold_ms' => 0, 'load_ms' => 0, 'run_ms' => 0],
            'verdict' => $metrics['verdict'] ?? null,
            'window' => $data['window'] ?? null,
        ];
        if (isset($metrics['costs'])) {
            $costs = self::object($metrics['costs'], 'costs');
            $manifest['profile'] = $costs['profile'] ?? null;
            $manifest['costs_file'] = $costs['costs_file'] ?? null;
            $manifest['costs_file_hash'] = $costs['costs_file_hash'] ?? null;
            $manifest['profiles_id'] = $profilesId;
        }
        if (isset($metrics['instruments']) && is_array($metrics['instruments'])) {
            $manifest['instruments_file'] = $metrics['instruments']['file'] ?? null;
            $manifest['instruments_hash'] = $metrics['instruments']['instruments_hash'] ?? null;
        }

        return $manifest;
    }

    /** Copie du golden, avec un manifeste s11 dans chaque run qui n'en porte pas. */
    private function goldenRun(): RunFixture
    {
        $run = $this->run = RunFixture::copyLines(self::GOLDEN);
        $zero = 'blake3:' . str_repeat('0', 64);
        $lines = self::json($run->path('lines.json'));

        foreach (self::object($lines['lines'] ?? null, 'lines') as $index => $line) {
            $line = self::object($line, 'lines[' . $index . ']');
            $dir = $line['dir'] ?? null;
            if (!is_string($dir)) {
                throw new RuntimeException('lines[' . $index . '].dir attendu');
            }
            if (is_file($run->path($dir . '/profiles.json'))) {
                $profiles = self::json($run->path($dir . '/profiles.json'));
                foreach (self::object($profiles['profiles'] ?? null, 'profiles') as $summary) {
                    $summary = self::object($summary, 'profiles[]');
                    $profileDir = $summary['dir'] ?? null;
                    if (!is_string($profileDir)) {
                        throw new RuntimeException('profiles[].dir attendu');
                    }
                    $sub = $dir . '/' . $profileDir;
                    if (is_file($run->path($sub . '/manifest.json'))) {
                        continue;
                    }
                    $manifest = self::manifest(
                        $run->path($sub . '/metrics.json'),
                        is_string($summary['run_id'] ?? null) ? $summary['run_id'] : '',
                        is_string($summary['result_hash'] ?? null) ? $summary['result_hash'] : $zero,
                        is_string($profiles['profiles_id'] ?? null) ? $profiles['profiles_id'] : null,
                    );
                    $run->write($sub . '/manifest.json', json_encode($manifest, JSON_THROW_ON_ERROR | JSON_PRETTY_PRINT));
                }
            } elseif (!is_file($run->path($dir . '/manifest.json'))) {
                $manifest = self::manifest(
                    $run->path($dir . '/metrics.json'),
                    is_string($line['run_id'] ?? null) ? $line['run_id'] : '',
                    $zero,
                    null,
                );
                $run->write($dir . '/manifest.json', json_encode($manifest, JSON_THROW_ON_ERROR | JSON_PRETTY_PRINT));
            }
        }

        return $run;
    }

    private function read(): LinesReport
    {
        $runs = new RunDirectoryReader(new TradesCsvReader(), new EquityCsvReader());
        $reader = new LinesDirectoryReader($runs, new ProfilesDirectoryReader($runs));

        return $reader->read($this->goldenRun()->directory);
    }

    public function testTheRealLinesDirectoryIsRead(): void
    {
        $report = $this->read();

        self::assertSame(['full', 'okx_usdc'], array_map(static fn ($l): string => $l->line, $report->lines));
        self::assertSame('tv4-lines-v1', LinesDirectoryReader::SCHEMA);
        self::assertNotSame([], $report->profileNames);
        self::assertSame($report->profileNames, array_keys($report->residual));
        foreach ($report->lines as $entry) {
            self::assertSame($report->profileNames, array_keys($entry->byProfile), $entry->line);
            self::assertSame($report->block, $entry->research()->block, $entry->line);
            self::assertSame($entry->line, $entry->research()->line);
        }
    }

    public function testEveryS11BlockOfTheRealOutputIsRead(): void
    {
        $report = $this->read();

        foreach ($report->lines as $entry) {
            $runs = null !== $entry->profiles ? $entry->profiles->runs : [LinesDirectoryReader::SINGLE => $entry->referenceRun()];
            foreach ($runs as $name => $run) {
                $where = $entry->line . '/' . $name;
                $metrics = $run->metrics;
                $research = $metrics->research;
                self::assertTrue($metrics->isResearch(), $where);
                self::assertNotNull($research, $where . ' : bloc research');
                self::assertNotSame([], $research->legs, $where . ' : jambes');
                self::assertNotNull($metrics->funnelByPair, $where . ' : bloc funnel_by_pair');
                self::assertNotNull($metrics->portfolio, $where . ' : bloc portfolio');
                self::assertNotNull($metrics->executability, $where . ' : bloc executability');
                self::assertSame('zero_volume_marked', $run->manifest->data->gapsPolicy, $where);
                self::assertSame($research->block, $run->manifest->data->block, $where);
                self::assertSame($research->purge->tradesKept, $metrics->nTrades, $where);
                self::assertCount($research->purge->tradesKept + $research->purge->tradesPurged, $run->trades, $where);
            }
        }
        // Mêmes configurations gelées sur les deux lignes (docs/specs/s11.md §9.1).
        $full = $report->line('full');
        $okx = $report->line('okx_usdc');
        foreach ($report->profileNames as $name) {
            self::assertSame(
                null !== $full->profiles ? $full->profiles->run($name)->manifest->configHash : $full->referenceRun()->manifest->configHash,
                null !== $okx->profiles ? $okx->profiles->run($name)->manifest->configHash : $okx->referenceRun()->manifest->configHash,
                $name,
            );
        }
    }

    public function testTheRealOutputIsRenderedWithoutAnyAbsentBlockNote(): void
    {
        $run = $this->goldenRun();
        $tester = new CommandTester((new Application(self::bootKernel()))->find('tv4:report'));

        $code = $tester->execute(['run_dir' => $run->directory, '--output' => $run->path('rapport.html')]);
        self::assertSame(0, $code, $tester->getDisplay());
        $html = $run->read('rapport.html');

        self::assertStringNotContainsString('Ce run ne porte pas de bloc', $html);
        self::assertSame(1, substr_count($html, 'id="lignes"'));
        self::assertSame(1, substr_count($html, 'id="recherche"'));
        self::assertStringContainsString('Survivance résiduelle', $html);
        self::assertStringContainsString('Refus d\'entrée de la recherche', $html);
    }
}
