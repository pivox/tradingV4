<?php

declare(strict_types=1);

namespace App\Tests\Run;

use App\Run\Csv\EquityCsvReader;
use App\Run\Csv\TradesCsvReader;
use App\Run\Dto\ProfilesReport;
use App\Run\ProfilesDirectoryReader;
use App\Run\RunDirectoryReader;
use App\Tests\Support\RunFixture;
use RuntimeException;
use Symfony\Bundle\FrameworkBundle\Console\Application;
use Symfony\Bundle\FrameworkBundle\Test\KernelTestCase;
use Symfony\Component\Console\Tester\CommandTester;

/**
 * Lit la vraie sortie de `tv4 backtest --costs` : le golden `tv4/tests/golden_runs/portfolio_profiles/` de
 * docs/specs/s10.md §16 (`profiles.json` et les sous-dossiers `baseline/`, `adverse/` et `maker/`).
 *
 * Le golden n'a pas de manifeste (comme `portfolio_2legs`, il ne contient que des fichiers déterministes) : le
 * test en écrit un par profil dans un dossier temporaire, de la forme du manifeste réel (schéma
 * `tv4-manifest-s10`), dont les hashes sont ceux de `profiles.json` et du `metrics.json` du profil, parce que le
 * lecteur les recoupe. Un sous-dossier qui porte déjà son `manifest.json` le garde.
 *
 * Ce test garde les clés que suppose le lecteur : un bloc renommé côté Rust, puis un golden régénéré, le ferait
 * échouer. Aucune valeur numérique du golden n'y est épinglée, il peut être régénéré.
 */
final class GoldenProfilesRunTest extends KernelTestCase
{
    private const GOLDEN = __DIR__ . '/../../../tv4/tests/golden_runs/portfolio_profiles';

    private ?RunFixture $run = null;

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

    /** Copie du golden, avec un manifeste s10 de la forme que tv4 écrit dans les sous-dossiers qui n'en ont pas. */
    private function goldenRun(): RunFixture
    {
        $run = $this->run = RunFixture::copyProfiles(self::GOLDEN);
        $profiles = self::json($run->path('profiles.json'));
        $zero = 'blake3:' . str_repeat('0', 64);

        foreach (self::object($profiles['profiles'] ?? null, 'profiles') as $index => $summary) {
            $summary = self::object($summary, 'profiles[' . $index . ']');
            $dir = $summary['dir'] ?? null;
            if (!is_string($dir) || is_file($run->path($dir . '/manifest.json'))) {
                continue;
            }
            $metrics = self::json($run->path($dir . '/metrics.json'));
            $data = self::object($metrics['data'] ?? null, 'data');
            $config = self::object($metrics['config'] ?? null, 'config');
            $costs = self::object($metrics['costs'] ?? null, 'costs');
            $instruments = self::object($metrics['instruments'] ?? null, 'instruments');
            $manifest = [
                'schema' => 'tv4-manifest-s10',
                'run_id' => $summary['run_id'] ?? null,
                'config' => $config,
                'config_hash' => $metrics['config_hash'] ?? null,
                'data' => $data,
                'dataset_hash' => $metrics['dataset_hash'] ?? null,
                'hash_format' => $data['hash_format'] ?? null,
                'initial_cash' => $config['initial_cash'] ?? null,
                'instruments_file' => $instruments['file'] ?? null,
                'instruments_hash' => $instruments['instruments_hash'] ?? null,
                'profile' => $costs['profile'] ?? null,
                'costs_file' => $costs['costs_file'] ?? null,
                'costs_file_hash' => $costs['costs_file_hash'] ?? null,
                'profiles_id' => $profiles['profiles_id'] ?? null,
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
                'result_hash' => $summary['result_hash'] ?? null,
                'started_at' => '2026-10-28T00:00:00Z',
                'timings' => ['cli_cold_ms' => 0, 'load_ms' => 0, 'run_ms' => 0],
                'verdict' => $metrics['verdict'] ?? null,
                'window' => $data['window'] ?? null,
            ];
            $run->write($dir . '/manifest.json', json_encode($manifest, JSON_THROW_ON_ERROR | JSON_PRETTY_PRINT));
        }

        return $run;
    }

    private function read(): ProfilesReport
    {
        $reader = new ProfilesDirectoryReader(new RunDirectoryReader(new TradesCsvReader(), new EquityCsvReader()));

        return $reader->read($this->goldenRun()->directory);
    }

    public function testTheRealProfilesDirectoryIsRead(): void
    {
        $report = $this->read();

        self::assertSame(['baseline', 'adverse', 'maker'], array_keys($report->runs));
        self::assertContains($report->label, ProfilesDirectoryReader::LABELS);
        self::assertContains($report->maker->status, ProfilesDirectoryReader::MAKER_STATUSES);
        self::assertCount(3, $report->profiles);
    }

    public function testEveryS10BlockOfTheRealOutputIsRead(): void
    {
        $report = $this->read();

        foreach ($report->runs as $name => $run) {
            $metrics = $run->metrics;
            self::assertTrue($metrics->isCosts(), $name);
            self::assertNotNull($metrics->costs, $name . ' : bloc costs');
            self::assertNotNull($metrics->costArithmetic, $name . ' : bloc cost_arithmetic');
            self::assertNotNull($metrics->costCounters, $name . ' : bloc cost_counters');
            self::assertSame($name, $metrics->costs->profile);
            self::assertNotSame([], $metrics->costs->legs, $name . ' : jambes');
            self::assertNotSame([], $metrics->costCounters->byPair, $name . ' : compteurs par paire');
            self::assertNull($run->manifest->config->slippageBps);
            self::assertSame($metrics->costs->feeGrid, $run->manifest->config->feesExchange);
            self::assertNotNull($metrics->funnelByPair, $name . ' : bloc funnel_by_pair');
            self::assertNotNull($metrics->portfolio, $name . ' : bloc portfolio');
            self::assertNotNull($metrics->executability, $name . ' : bloc executability');
        }
        $baseline = $report->baseline()->metrics->costs;
        $maker = $report->run('maker')->metrics->costs;
        self::assertNotNull($baseline);
        self::assertNotNull($maker);
        self::assertSame('taker', $baseline->legs[0]->entry);
        self::assertSame('maker', $maker->legs[0]->entry);
        self::assertTrue($baseline->gatesProfile);
        self::assertFalse($maker->gatesProfile);
    }

    public function testTheRealOutputIsRenderedWithoutAnyAbsentBlockNote(): void
    {
        $run = $this->goldenRun();
        $tester = new CommandTester((new Application(self::bootKernel()))->find('tv4:report'));

        $code = $tester->execute(['run_dir' => $run->directory, '--output' => $run->path('rapport.html')]);
        self::assertSame(0, $code, $tester->getDisplay());
        $html = $run->read('rapport.html');

        self::assertStringNotContainsString('Ce run ne porte pas de bloc', $html);
        self::assertSame(1, substr_count($html, 'id="profils"'));
        self::assertSame(1, substr_count($html, 'id="couts"'));
        self::assertStringContainsString('Étiquette :', $html);
        self::assertStringContainsString('Compteurs de coûts', $html);
        self::assertStringContainsString('Arithmétique du coût (04 §3.3)', $html);
    }
}
