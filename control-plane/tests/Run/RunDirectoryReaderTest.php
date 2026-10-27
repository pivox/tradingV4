<?php

declare(strict_types=1);

namespace App\Tests\Run;

use App\Run\Csv\EquityCsvReader;
use App\Run\Csv\TradesCsvReader;
use App\Run\InvalidRunException;
use App\Run\RunDirectoryReader;
use App\Tests\Support\RunFixture;
use PHPUnit\Framework\Attributes\DataProvider;
use PHPUnit\Framework\TestCase;

final class RunDirectoryReaderTest extends TestCase
{
    private ?RunFixture $copy = null;

    protected function tearDown(): void
    {
        $this->copy?->cleanup();
        $this->copy = null;
    }

    private function reader(): RunDirectoryReader
    {
        return new RunDirectoryReader(new TradesCsvReader(), new EquityCsvReader());
    }

    private function copy(): RunFixture
    {
        return $this->copy = RunFixture::copy();
    }

    public function testReadsTheFixtureIntoTypedDtos(): void
    {
        $run = $this->reader()->read(RunFixture::SOURCE);

        self::assertSame(str_repeat('5e7f1a2b', 8), $run->manifest->runId);
        self::assertSame('dsh-v1', $run->manifest->hashFormat);
        self::assertSame('sma_cross', $run->manifest->config->strategyId);
        self::assertSame('1.1.0', $run->manifest->config->strategyVersion);
        self::assertSame(20, $run->manifest->config->strategyParams['fast']);
        self::assertSame(300.0, $run->manifest->config->initialCash);
        self::assertSame('x86-64-v3', $run->manifest->engine->targetCpu);
        self::assertSame('2021-01-01T04:10:00Z', $run->manifest->window->start);
        self::assertSame('1h', $run->manifest->equityStep);
        self::assertSame(398, $run->manifest->timings->cliColdMs);

        self::assertSame(10, $run->metrics->nTrades);
        self::assertSame('rejected_too_few_trades', $run->metrics->verdict);
        self::assertSame(30, $run->metrics->minTrades);
        self::assertSame(11520, $run->metrics->windowBars);
        self::assertSame(1, $run->metrics->signalsRejected);
        self::assertSame(2, $run->metrics->sim['bars_zero_volume_blocked']);
        self::assertNotNull($run->metrics->distribution->histogramR);
        self::assertCount(30, $run->metrics->distribution->histogramR->counts);
        self::assertCount(4, $run->metrics->distribution->pnlWithoutBest);
        self::assertNull($run->metrics->distribution->pnlWithoutBest[3]->pnlNet);
        self::assertSame(10, $run->metrics->funnel->entryFills);
        self::assertSame(14, $run->metrics->funnel->rawEntrySignals);
        self::assertSame(11, $run->metrics->funnel->entrySignals);
        self::assertSame(['stop_not_positive' => 1, 'trend_filter' => 2], $run->metrics->funnel->entryFilteredByReason);

        self::assertCount(10, $run->trades);
        self::assertSame(1, $run->trades[0]->lotId);
        self::assertSame('strategy_exit', $run->trades[0]->exitType);
        self::assertSame('', $run->trades[1]->exitTag);
        self::assertNull($run->trades[6]->stopAtEntry);
        self::assertNull($run->trades[6]->rMultiple);
        self::assertNull($run->trades[0]->regimeAtEntry);
        self::assertNull($run->trades[0]->spreadCost);
        self::assertTrue($run->trades[9]->forcedClose);

        self::assertSame(193, $run->equity->count());
        self::assertSame(300.0, $run->equity->equity[0]);
        foreach ($run->equity->drawdown as $drawdown) {
            self::assertLessThanOrEqual(0.0, $drawdown);
        }
    }

    public function testMissingDirectory(): void
    {
        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('dossier de run introuvable');
        $this->reader()->read(sys_get_temp_dir() . '/tv4-n-existe-pas');
    }

    /**
     * @return iterable<string, array{string}>
     */
    public static function requiredFiles(): iterable
    {
        foreach (RunFixture::FILES as $file) {
            yield $file => [$file];
        }
    }

    #[DataProvider('requiredFiles')]
    public function testMissingFileNamesTheFile(string $file): void
    {
        $copy = $this->copy();
        $copy->remove($file);

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage($file . ' : fichier absent');
        $this->reader()->read($copy->directory);
    }

    /**
     * @return iterable<string, array{string}>
     */
    public static function jsonFiles(): iterable
    {
        yield 'manifest.json' => ['manifest.json'];
        yield 'metrics.json' => ['metrics.json'];
    }

    #[DataProvider('jsonFiles')]
    public function testInvalidJson(string $file): void
    {
        $copy = $this->copy();
        $copy->write($file, '{"schema": ');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage($file . ' : JSON invalide');
        $this->reader()->read($copy->directory);
    }

    public function testJsonThatIsNotAnObject(): void
    {
        $copy = $this->copy();
        $copy->write('metrics.json', '[1, 2]');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json');
        $this->reader()->read($copy->directory);
    }

    public function testUnknownManifestSchema(): void
    {
        $copy = $this->copy();
        $copy->replace('manifest.json', '"tv4-manifest-s8"', '"tv4-manifest-s7"');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('manifest.json : schéma inconnu : "tv4-manifest-s7" (attendu "tv4-manifest-s8")');
        $this->reader()->read($copy->directory);
    }

    public function testUnknownMetricsSchema(): void
    {
        $copy = $this->copy();
        $copy->replace('metrics.json', '"tv4-metrics-s8"', '"tv4-metrics-s6"');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json : schéma inconnu : "tv4-metrics-s6"');
        $this->reader()->read($copy->directory);
    }

    public function testMissingSchemaKey(): void
    {
        $copy = $this->copy();
        $copy->write('metrics.json', '{"n_trades": 3}');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json : clé absente à schema');
        $this->reader()->read($copy->directory);
    }

    public function testMissingNestedKeyNamesTheJsonPath(): void
    {
        $copy = $this->copy();
        $copy->replace('manifest.json', '"git_sha"', '"git_sha_renomme"');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('manifest.json : clé absente à engine.git_sha');
        $this->reader()->read($copy->directory);
    }

    public function testWrongTypeNamesThePathAndTheTypes(): void
    {
        $copy = $this->copy();
        $copy->replace('metrics.json', "\"n_trades\": 10,\n    \"n_trades_forced\"", "\"n_trades\": \"dix\",\n    \"n_trades_forced\"");

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json : n_trades doit être un entier, trouvé une chaîne');
        $this->reader()->read($copy->directory);
    }

    public function testInconsistentConfigHashBetweenFiles(): void
    {
        $copy = $this->copy();
        $copy->replace('metrics.json', '"config_hash": "' . str_repeat('c0de5a1e', 8) . '"', '"config_hash": "' . str_repeat('00000000', 8) . '"');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('config_hash : différent de celui du manifeste');
        $this->reader()->read($copy->directory);
    }

    public function testTradesHeaderMustBeExact(): void
    {
        $copy = $this->copy();
        $copy->replace('trades.csv', 'lot_id,entry_signal_ms', 'lot,entry_signal_ms');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('trades.csv : en-tête inattendu');
        $this->reader()->read($copy->directory);
    }

    public function testEquityHeaderMustBeExact(): void
    {
        $copy = $this->copy();
        $copy->replace('equity.csv', 'time_ms,equity,drawdown', 'time_ms,equity');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('equity.csv : en-tête inattendu');
        $this->reader()->read($copy->directory);
    }

    public function testTradesRowWithTooFewFields(): void
    {
        $copy = $this->copy();
        $lines = explode("\n", $copy->read('trades.csv'));
        $lines[2] = '2,1,2,3';
        $copy->write('trades.csv', implode("\n", $lines));

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('trades.csv : ligne 3 : 36 champs attendus, 4 trouvés');
        $this->reader()->read($copy->directory);
    }

    public function testNonNumericValueNamesLineAndColumn(): void
    {
        $copy = $this->copy();
        $copy->replace('trades.csv', ',29250.02,', ',beaucoup,');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('trades.csv : ligne 2 : colonne entry_price : nombre attendu, trouvé "beaucoup"');
        $this->reader()->read($copy->directory);
    }

    public function testExponentNotationIsRejected(): void
    {
        $copy = $this->copy();
        $copy->replace('equity.csv', '298.82099094', '2.9882099094e2');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('equity.csv : ligne 3 : colonne equity : nombre attendu');
        $this->reader()->read($copy->directory);
    }

    public function testForbiddenEmptyField(): void
    {
        $copy = $this->copy();
        $copy->replace('trades.csv', ',29250.02,', ',,');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('trades.csv : ligne 2 : colonne entry_price : valeur vide interdite');
        $this->reader()->read($copy->directory);
    }

    public function testUnknownExitType(): void
    {
        $copy = $this->copy();
        $copy->replace('trades.csv', ',end_of_window,', ',roll,');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('trades.csv : ligne 11 : colonne exit_type : valeur inconnue "roll"');
        $this->reader()->read($copy->directory);
    }

    public function testInvalidBooleanAndInteger(): void
    {
        $copy = $this->copy();
        $copy->replace('trades.csv', ',true,false,sma_cross_up,sma_cross_down,BTC-USD,1609477260000', ',oui,false,sma_cross_up,sma_cross_down,BTC-USD,1609477260000');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('colonne capped_by_cash : "true" ou "false" attendu');
        $this->reader()->read($copy->directory);
    }

    public function testNonIntegerTimestamp(): void
    {
        $copy = $this->copy();
        $copy->replace('equity.csv', '1609480800000,', '1609480800000.5,');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('equity.csv : ligne 3 : colonne time_ms : entier attendu');
        $this->reader()->read($copy->directory);
    }
}
