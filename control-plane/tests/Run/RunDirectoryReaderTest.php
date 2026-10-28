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
        self::assertSame(12270, $run->metrics->dataBars);
        self::assertSame(['BTC-USD'], $run->manifest->data->symbols);
        self::assertSame('BTC-USD', $run->manifest->data->symbolsLabel());
        self::assertSame('tv4-manifest-s8', $run->manifest->schema);
        self::assertSame('tv4-metrics-s8', $run->metrics->schema);
        self::assertFalse($run->metrics->isPortfolio());
        self::assertNull($run->metrics->funnel->entryRejectedMaxPositions);
        self::assertNull($run->metrics->signalsRejectedMaxPositions);
        self::assertIsArray($run->manifest->config->instrument);
        self::assertNull($run->manifest->config->instruments);
        self::assertNull($run->manifest->config->portfolio);
        self::assertNull($run->manifest->instrumentsHash);
        self::assertNull($run->metrics->funnelByPair);
        self::assertNull($run->metrics->portfolio);
        self::assertNull($run->metrics->priority);
        self::assertNull($run->metrics->regime);
        self::assertNull($run->metrics->instruments);
        self::assertNull($run->metrics->executability);
        self::assertNull($run->metrics->g1Executable);
        self::assertNull($run->metrics->distribution->byPair);
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
        $this->expectExceptionMessage('manifest.json : schéma inconnu : "tv4-manifest-s7" (attendu "tv4-manifest-s8" ou "tv4-manifest-s9" ou "tv4-manifest-s10")');
        $this->reader()->read($copy->directory);
    }

    public function testUnknownMetricsSchema(): void
    {
        $copy = $this->copy();
        $copy->replace('metrics.json', '"tv4-metrics-s8"', '"tv4-metrics-s6"');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json : schéma inconnu : "tv4-metrics-s6" (attendu "tv4-metrics-s8" ou "tv4-metrics-s9" ou "tv4-metrics-s10")');
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

    public function testReadsThePortfolioFixtureIntoTypedDtos(): void
    {
        $run = $this->reader()->read(RunFixture::PORTFOLIO_SOURCE);

        self::assertSame('tv4-manifest-s9', $run->manifest->schema);
        self::assertSame('tv4-metrics-s9', $run->metrics->schema);
        self::assertTrue($run->metrics->isPortfolio());
        self::assertSame(['BTC-USD', 'ETH-USD'], $run->manifest->data->symbols);
        self::assertSame('BTC-USD, ETH-USD', $run->manifest->data->symbolsLabel());
        self::assertSame('kraken', $run->manifest->data->exchange);
        // Forme réelle de tv4 : un run portefeuille vient toujours d'un dataset (`--symbols` exige `--dataset`).
        self::assertSame('dataset', $run->manifest->data->source);
        self::assertSame('exemple_portefeuille', $run->manifest->data->dataset);
        self::assertNull($run->manifest->data->bars);
        self::assertNull($run->manifest->data->regimeSymbol);
        self::assertCount(2, $run->manifest->data->series);
        self::assertSame('ETH-USD', $run->manifest->data->series[1]->symbol);
        self::assertSame(12254, $run->manifest->data->series[1]->rows);
        self::assertSame('dsh-v1', $run->manifest->hashFormat);
        self::assertSame(str_repeat('c0de9001', 8), $run->manifest->configHash);
        self::assertSame('2021-01-01T04:10:00Z', $run->manifest->window->start);

        // Champs de s8 absents du bloc `data` de s9 : nuls, pas une erreur.
        self::assertNull($run->metrics->dataBars);
        self::assertNull($run->metrics->dataSyntheticBars);
        self::assertNull($run->metrics->minutesMissing);
        self::assertNull($run->metrics->windowBars);
        self::assertNull($run->metrics->windowStartIndex);
        self::assertSame(10, $run->metrics->nTrades);

        self::assertNull($run->manifest->config->instrument);
        self::assertNotNull($run->manifest->config->instruments);
        self::assertSame(0.001, $run->manifest->config->instruments['ETH-USD']['min_qty']);
        self::assertNotNull($run->manifest->config->portfolio);
        self::assertSame('["BTC-USD","ETH-USD"]', $run->manifest->config->portfolio['universe']);
        self::assertSame(300.0, $run->manifest->config->initialCash);
        self::assertSame('blake3:' . str_repeat('1257900c', 8), $run->manifest->instrumentsHash);
        self::assertSame('instruments/okx/spot-20261028T124500Z.json', $run->manifest->instrumentsFile);

        $byPair = $run->metrics->funnelByPair;
        self::assertNotNull($byPair);
        self::assertSame(['BTC-USD', 'ETH-USD'], array_map(static fn ($p): string => $p->symbol, $byPair));
        self::assertSame(1, $byPair[0]->rejected['cash']);
        self::assertSame(1, $byPair[1]->rejected['min_notional']);
        self::assertSame(1, $byPair[0]->rejected['max_positions']);
        self::assertSame(2, $byPair[1]->rejected['max_positions']);
        self::assertSame($run->metrics->funnel->entryFills, array_sum(array_map(static fn ($p): int => $p->entryFills, $byPair)));
        self::assertSame($run->metrics->funnel->entrySignals, array_sum(array_map(static fn ($p): int => $p->entrySignals, $byPair)));
        self::assertSame($run->metrics->funnel->evaluations, array_sum(array_map(static fn ($p): int => $p->evaluations, $byPair)));

        // Refus du plafond de positions : lus en s9, identités de l'entonnoir gardées (comme `tv4 verify`).
        $funnel = $run->metrics->funnel;
        self::assertSame(3, $funnel->entryRejectedMaxPositions);
        self::assertSame(3, $run->metrics->signalsRejectedMaxPositions);
        self::assertSame(
            $funnel->entrySignals,
            $funnel->entryOrders + array_sum($funnel->entryRejectedByReason) + $funnel->entryRejectedMaxPositions,
        );
        self::assertSame(
            $run->metrics->signalsRejected,
            array_sum($run->metrics->rejectedByReason) + $run->metrics->signalsRejectedMaxPositions,
        );
        self::assertSame($run->metrics->signalsEmitted - $run->metrics->signalsAccepted, $run->metrics->signalsRejected);
        self::assertSame($run->metrics->portfolio?->positionsCapHits, $funnel->entryRejectedMaxPositions);

        self::assertNotNull($run->metrics->portfolio);
        self::assertSame(2, $run->metrics->portfolio->legs);
        self::assertSame(1, $run->metrics->portfolio->quoteGapExits);
        self::assertSame('research', $run->metrics->portfolio->sizing);
        self::assertNotNull($run->metrics->priority);
        self::assertSame('kraken', $run->metrics->priority->rankExchange);
        self::assertNull($run->metrics->regime);

        $instruments = $run->metrics->instruments;
        self::assertNotNull($instruments);
        self::assertSame('okx', $instruments->source);
        self::assertSame(['ETH-USDC'], $instruments->notLive);
        self::assertCount(1, $instruments->upcomingChanges);
        self::assertSame('minSz', $instruments->upcomingChanges[0]->param);
        self::assertSame(1793600000000, $instruments->upcomingChanges[0]->effTimeMs());

        $ex = $run->metrics->executability;
        self::assertNotNull($ex);
        self::assertSame(300.0, $ex->referenceCapital);
        self::assertFalse($ex->venueMatchesExecution);
        self::assertSame(['BTC-USD'], $ex->liveList);
        self::assertSame('rejected_volume', $ex->rejected[0]->reason);
        self::assertSame('executable', $ex->pairs[0]->verdict);
        self::assertSame(0.0001, $ex->pairs[0]->minSz);
        self::assertSame('suspend', $ex->pairs[1]->state);

        $g1 = $run->metrics->g1Executable;
        self::assertNotNull($g1);
        self::assertSame('fail', $g1->verdict);
        self::assertTrue($g1->atReferenceCapital);
        self::assertSame(10, $g1->counts['fills']);
        self::assertSame(0.95, $g1->threshold);

        $pairs = $run->metrics->distribution->byPair;
        self::assertNotNull($pairs);
        self::assertSame(['BTC-USD', 'ETH-USD'], array_map(static fn ($p): string => $p->symbol, $pairs));
        self::assertEqualsWithDelta($run->metrics->pnlNet, $pairs[0]->netPnl + $pairs[1]->netPnl, 1e-9);

        self::assertCount(10, $run->trades);
        self::assertSame(['BTC-USD', 'ETH-USD'], array_values(array_unique(array_map(static fn ($t): string => $t->symbol, $run->trades))));
        self::assertSame('quote_gap', $run->trades[8]->exitReason);
        self::assertSame('gap_forced_exit', $run->trades[8]->exitType);
        self::assertFalse($run->trades[8]->forcedClose);
    }

    public function testMetricsSchemaMustMatchTheManifestGeneration(): void
    {
        $copy = $this->copy();
        $copy->replace('metrics.json', '"tv4-metrics-s8"', '"tv4-metrics-s9"');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json : schema : "tv4-metrics-s9" ne correspond pas au schéma du manifeste "tv4-manifest-s8"');
        $this->reader()->read($copy->directory);
    }

    public function testPortfolioManifestWithAnS8MetricsFileIsRefused(): void
    {
        $copy = RunFixture::copyPortfolio();
        $this->copy = $copy;
        $copy->replace('metrics.json', '"tv4-metrics-s9"', '"tv4-metrics-s8"');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('ne correspond pas au schéma du manifeste "tv4-manifest-s9"');
        $this->reader()->read($copy->directory);
    }

    public function testUnknownFutureSchemaIsRefused(): void
    {
        $copy = RunFixture::copyPortfolio();
        $this->copy = $copy;
        $copy->replace('manifest.json', '"tv4-manifest-s9"', '"tv4-manifest-s11"');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('manifest.json : schéma inconnu : "tv4-manifest-s11" (attendu "tv4-manifest-s8" ou "tv4-manifest-s9" ou "tv4-manifest-s10")');
        $this->reader()->read($copy->directory);
    }

    public function testS8StaysStrictAboutTheSinglePairKey(): void
    {
        $copy = $this->copy();
        $copy->replace('manifest.json', '"symbol": "BTC-USD"', '"symbols": ["BTC-USD"]');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('manifest.json : clé absente à data.symbol');
        $this->reader()->read($copy->directory);
    }

    public function testPortfolioManifestRequiresTheUniverse(): void
    {
        $copy = RunFixture::copyPortfolio();
        $this->copy = $copy;
        $copy->replace('manifest.json', '"symbols": [', '"univers": [');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('manifest.json : clé absente à data.symbols');
        $this->reader()->read($copy->directory);
    }

    public function testPortfolioHashesAndWindowFallBackToTheDataBlock(): void
    {
        $copy = RunFixture::copyPortfolio();
        $this->copy = $copy;
        foreach (['window', 'dataset_hash', 'hash_format'] as $key) {
            $copy->unsetJson('manifest.json', [$key]);
        }
        $copy->unsetJson('metrics.json', ['dataset_hash']);

        $run = $this->reader()->read($copy->directory);

        self::assertSame('blake3:' . str_repeat('da7a9001', 8), $run->manifest->datasetHash);
        self::assertSame('blake3:' . str_repeat('da7a9001', 8), $run->metrics->datasetHash);
        self::assertSame('dsh-v1', $run->manifest->hashFormat);
        self::assertSame(1610165400000, $run->manifest->window->endMs);
    }

    public function testPortfolioBlocksMayBeNullOrAbsent(): void
    {
        $copy = RunFixture::copyPortfolio();
        $this->copy = $copy;
        $copy->setJson('metrics.json', ['executability'], null);
        $copy->setJson('metrics.json', ['g1_executable'], null);
        foreach ([['priority'], ['funnel_by_pair'], ['instruments'], ['portfolio'], ['distribution', 'by_pair']] as $path) {
            $copy->unsetJson('metrics.json', $path);
        }

        $run = $this->reader()->read($copy->directory);

        self::assertNull($run->metrics->executability);
        self::assertNull($run->metrics->g1Executable);
        self::assertNull($run->metrics->priority);
        self::assertNull($run->metrics->funnelByPair);
        self::assertNull($run->metrics->instruments);
        self::assertNull($run->metrics->portfolio);
        self::assertNull($run->metrics->distribution->byPair);
    }

    public function testPortfolioRequiresTheMaxPositionsCounters(): void
    {
        $copy = RunFixture::copyPortfolio();
        $this->copy = $copy;
        $copy->unsetJson('metrics.json', ['funnel', 'entry_rejected_max_positions']);

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json : clé absente à funnel.entry_rejected_max_positions');
        $this->reader()->read($copy->directory);
    }

    public function testPortfolioRequiresTheSignalsMaxPositionsCounter(): void
    {
        $copy = RunFixture::copyPortfolio();
        $this->copy = $copy;
        $copy->unsetJson('metrics.json', ['signals', 'rejected_max_positions']);

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json : clé absente à signals.rejected_max_positions');
        $this->reader()->read($copy->directory);
    }

    public function testRegimeBlockIsRead(): void
    {
        $copy = RunFixture::copyPortfolio();
        $this->copy = $copy;
        $copy->replace(
            'metrics.json',
            "\"regime\": null,\n    \"return_total\"",
            "\"regime\": {\"days_bear\": 3, \"days_bull\": 5, \"first_ready_ms\": 1608249600000, \"sma_period\": 200, \"symbol\": \"BTC-USD\", \"tf\": \"1d\"},\n    \"return_total\"",
        );

        $regime = $this->reader()->read($copy->directory)->metrics->regime;

        self::assertNotNull($regime);
        self::assertSame('BTC-USD', $regime->symbol);
        self::assertSame(200, $regime->smaPeriod);
        self::assertSame(1608249600000, $regime->firstReadyMs);
        self::assertSame(5, $regime->daysBull);
        self::assertSame(3, $regime->daysBear);
    }

    public function testInstrumentsHashMustAgreeBetweenManifestAndMetrics(): void
    {
        $copy = RunFixture::copyPortfolio();
        $this->copy = $copy;
        $copy->setJson('manifest.json', ['instruments_hash'], 'blake3:' . str_repeat('00000000', 8));

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('instruments.instruments_hash : différent de celui du manifeste');
        $this->reader()->read($copy->directory);
    }

    public function testWrongTypeInAPortfolioBlockNamesThePath(): void
    {
        $copy = RunFixture::copyPortfolio();
        $this->copy = $copy;
        $copy->replace('metrics.json', '"legs": 2', '"legs": "deux"');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json : portfolio.legs doit être un entier, trouvé une chaîne');
        $this->reader()->read($copy->directory);
    }

    public function testNumericStringsAreAcceptedForOkxValues(): void
    {
        $copy = RunFixture::copyPortfolio();
        $this->copy = $copy;
        $copy->replace('metrics.json', '"min_sz": 0.0001', '"min_sz": "0.0001"');

        $ex = $this->reader()->read($copy->directory)->metrics->executability;

        self::assertNotNull($ex);
        self::assertSame(0.0001, $ex->pairs[0]->minSz);
    }

    public function testQuoteGapAndGapForcedExitAreAcceptedInTrades(): void
    {
        $copy = $this->copy();
        $copy->replace('trades.csv', ',forced_close,taker,', ',quote_gap,taker,');
        $copy->replace('trades.csv', ',end_of_window,', ',gap_forced_exit,');

        $run = $this->reader()->read($copy->directory);

        self::assertSame('quote_gap', $run->trades[9]->exitReason);
        self::assertSame('gap_forced_exit', $run->trades[9]->exitType);
    }

    public function testUnknownExitReasonIsStillRefused(): void
    {
        $copy = $this->copy();
        $copy->replace('trades.csv', ',forced_close,taker,', ',quote_gapp,taker,');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('colonne exit_reason : valeur inconnue "quote_gapp"');
        $this->reader()->read($copy->directory);
    }

    public function testNumericOkxChangesAreRenderedAsStrings(): void
    {
        $copy = RunFixture::copyPortfolio();
        $this->copy = $copy;
        $copy->replace('metrics.json', '"new_value": "0.001"', '"new_value": 0.001');
        $copy->replace('metrics.json', '"eff_time": "1793600000000"', '"eff_time": "prochainement"');

        $change = $this->reader()->read($copy->directory)->metrics->instruments?->upcomingChanges[0];

        self::assertNotNull($change);
        self::assertSame('0.001', $change->newValue);
        self::assertSame('prochainement', $change->effTime);
        self::assertNull($change->effTimeMs());
    }
}
