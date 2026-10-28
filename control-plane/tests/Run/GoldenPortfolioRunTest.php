<?php

declare(strict_types=1);

namespace App\Tests\Run;

use App\Run\Csv\EquityCsvReader;
use App\Run\Csv\TradesCsvReader;
use App\Run\Dto\RunReport;
use App\Run\RunDirectoryReader;
use App\Tests\Support\RunFixture;
use RuntimeException;
use Symfony\Bundle\FrameworkBundle\Console\Application;
use Symfony\Bundle\FrameworkBundle\Test\KernelTestCase;
use Symfony\Component\Console\Tester\CommandTester;

/**
 * Lit la vraie sortie de `tv4` : le golden `tv4/tests/golden_runs/portfolio_2legs/` (metrics.json, trades.csv,
 * equity.csv). Le golden n'a pas de manifeste (`le_golden_est_complet_et_sans_manifeste`) : le test en écrit un
 * dans un dossier temporaire, de la forme du manifeste réel (schéma `tv4-manifest-s9`), dont les hashes sont ceux
 * du golden, parce que le lecteur les recoupe.
 *
 * Ce test garde les clés de bloc que suppose le lecteur : un bloc renommé côté Rust, puis un golden régénéré, le
 * ferait échouer. Aucune valeur numérique du golden n'y est épinglée, il peut être régénéré.
 */
final class GoldenPortfolioRunTest extends KernelTestCase
{
    private const GOLDEN = __DIR__ . '/../../../tv4/tests/golden_runs/portfolio_2legs';

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

    /** Manifeste s9 de la forme que tv4 écrit, construit sur le `metrics.json` du golden. */
    private function goldenRun(): RunFixture
    {
        $json = file_get_contents(self::GOLDEN . '/metrics.json');
        if (false === $json) {
            throw new RuntimeException('golden illisible : ' . self::GOLDEN . '/metrics.json');
        }
        $metrics = self::object(json_decode($json, true, 512, JSON_THROW_ON_ERROR), 'metrics.json');
        $data = self::object($metrics['data'] ?? null, 'data');
        $config = self::object($metrics['config'] ?? null, 'config');
        $instruments = self::object($metrics['instruments'] ?? null, 'instruments');

        $zero = 'blake3:' . str_repeat('0', 64);
        $manifest = [
            'schema' => 'tv4-manifest-s9',
            'run_id' => str_repeat('0', 64),
            'config' => $config,
            'config_hash' => $metrics['config_hash'] ?? null,
            'data' => $data,
            'dataset_hash' => $metrics['dataset_hash'] ?? null,
            'hash_format' => $data['hash_format'] ?? null,
            'initial_cash' => $config['initial_cash'] ?? null,
            'instruments_file' => $instruments['file'] ?? null,
            'instruments_hash' => $instruments['instruments_hash'] ?? null,
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
            'result_hash' => $zero,
            'started_at' => '2026-10-28T00:00:00Z',
            'timings' => ['cli_cold_ms' => 0, 'load_ms' => 0, 'run_ms' => 0],
            'verdict' => $metrics['verdict'] ?? null,
            'window' => $data['window'] ?? null,
        ];

        return $this->run = RunFixture::copyWithManifest(self::GOLDEN, $manifest);
    }

    private function read(): RunReport
    {
        return (new RunDirectoryReader(new TradesCsvReader(), new EquityCsvReader()))->read($this->goldenRun()->directory);
    }

    public function testEveryS9BlockOfTheRealOutputIsRead(): void
    {
        $metrics = $this->read()->metrics;

        self::assertTrue($metrics->isPortfolio());
        self::assertNotNull($metrics->funnelByPair, 'bloc funnel_by_pair');
        self::assertNotNull($metrics->portfolio, 'bloc portfolio');
        self::assertNotNull($metrics->priority, 'bloc priority');
        self::assertNotNull($metrics->instruments, 'bloc instruments');
        self::assertNotNull($metrics->executability, 'bloc executability');
        self::assertNotNull($metrics->g1Executable, 'bloc g1_executable');
        self::assertNotNull($metrics->distribution->byPair, 'bloc distribution.by_pair');
        // sma_cross ne lit pas le régime : le bloc vaut null dans la sortie de tv4.
        self::assertNull($metrics->regime, 'bloc regime');
    }

    public function testTheRealOutputCarriesTheMaxPositionsCountersAndTheDatasetSource(): void
    {
        $run = $this->read();
        $funnel = $run->metrics->funnel;

        self::assertSame('dataset', $run->manifest->data->source);
        self::assertNotNull($run->manifest->data->dataset);
        self::assertNotNull($funnel->entryRejectedMaxPositions);
        self::assertNotNull($run->metrics->signalsRejectedMaxPositions);
        // Identités de l'entonnoir : celles que contrôle `tv4 verify`.
        self::assertSame(
            $funnel->entrySignals,
            $funnel->entryOrders + array_sum($funnel->entryRejectedByReason) + $funnel->entryRejectedMaxPositions,
        );
        self::assertSame($run->metrics->signalsEmitted - $run->metrics->signalsAccepted, $run->metrics->signalsRejected);
        self::assertSame($funnel->entryRejectedMaxPositions, $run->metrics->signalsRejectedMaxPositions);
        self::assertSame($run->metrics->portfolio?->positionsCapHits, $funnel->entryRejectedMaxPositions);
    }

    public function testTheRealOutputIsRenderedWithoutAnyAbsentBlockNote(): void
    {
        $run = $this->goldenRun();
        $reader = new RunDirectoryReader(new TradesCsvReader(), new EquityCsvReader());
        $report = $reader->read($run->directory);

        $tester = new CommandTester((new Application(self::bootKernel()))->find('tv4:report'));
        $code = $tester->execute(['run_dir' => $run->directory, '--output' => $run->path('rapport.html')]);
        self::assertSame(0, $code, $tester->getDisplay());
        $html = $run->read('rapport.html');

        self::assertStringNotContainsString('Ce run ne porte pas de bloc', $html);
        // La branche réellement prise par un vrai run : « dataset <strong>nom</strong> ».
        self::assertStringContainsString('dataset <strong>' . $report->manifest->data->dataset . '</strong>', $html);
        self::assertStringNotContainsString('portefeuille <strong>', $html);
        self::assertStringContainsString('Capital initial (hors <code>config_hash</code>)', $html);
        self::assertStringContainsString('aucun : la stratégie ne lit pas le régime', $html);
        $hasRejections = 0 !== $report->metrics->funnel->entryRejectedMaxPositions;
        self::assertSame($hasRejections, str_contains($html, 'dont rejetés : <code>max_positions</code>'));
        self::assertSame($hasRejections, str_contains($html, 'rejet : <code>max_positions</code>'));
    }
}
