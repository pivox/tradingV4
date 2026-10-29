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

/**
 * Lecture d'un run s10 (`tv4-manifest-s10` et `tv4-metrics-s10`) : un sous-dossier de la fixture `run-profils/`.
 */
final class RunDirectoryReaderS10Test extends TestCase
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
        return $this->copy = RunFixture::copyProfiles();
    }

    public function testReadsTheBaselineRunIntoTypedDtos(): void
    {
        $run = $this->reader()->read(RunFixture::PROFILES_SOURCE . '/baseline');

        self::assertSame('tv4-manifest-s10', $run->manifest->schema);
        self::assertSame('tv4-metrics-s10', $run->metrics->schema);
        self::assertTrue($run->metrics->isCosts());
        self::assertTrue($run->metrics->isPortfolio());
        self::assertSame(['BTC-USD', 'ETH-USD'], $run->manifest->data->symbols);
        // En s10, la grille se lit dans costs.fee_grid et le slippage scalaire n'existe plus.
        self::assertSame('okx_regular', $run->manifest->config->feesExchange);
        self::assertSame(20.0, $run->manifest->config->makerBps);
        self::assertSame(35.0, $run->manifest->config->takerBps);
        self::assertNull($run->manifest->config->slippageBps);
        self::assertNotNull($run->manifest->config->instruments);
        self::assertNotNull($run->manifest->config->portfolio);
        self::assertSame('baseline', $run->manifest->profile);
        self::assertSame('config/costs.toml', $run->manifest->costsFile);
        self::assertSame(str_repeat('9f0f1e50', 8), $run->manifest->profilesId);
        self::assertNull($run->manifest->costMeasuresFile);
        self::assertNull($run->manifest->costMeasuresHash);
        self::assertSame(10, $run->metrics->nTrades);
        self::assertContains('stop_stress_uncalibrated', $run->metrics->simDeviations);
        self::assertContains('cost_defaults_unmeasured', $run->metrics->simDeviations);
        self::assertNotContains('stop_slippage_fixed', $run->metrics->simDeviations);
        self::assertNotNull($run->metrics->funnelByPair);
        self::assertNotNull($run->metrics->executability);

        $costs = $run->metrics->costs;
        self::assertNotNull($costs);
        self::assertSame('baseline', $costs->profile);
        self::assertTrue($costs->gatesProfile);
        self::assertSame('c1', $costs->rulesVersion);
        self::assertSame('okx_regular', $costs->feeGrid);
        self::assertSame(1, $costs->seed);
        self::assertFalse($costs->stressCalibrated);
        self::assertNull($costs->stressCalibrationRef);
        self::assertNull($costs->costMeasuresFile);
        self::assertCount(2, $costs->legs);
        $leg = $costs->legs[0];
        self::assertSame('BTC-USD', $leg->symbol);
        self::assertSame('major', $leg->class);
        self::assertSame('list', $leg->classSource);
        self::assertSame('default', $leg->spreadSource);
        self::assertSame(0.5, $leg->halfSpreadBps);
        self::assertSame(['p50' => 5.0, 'p95' => 50.0, 'p99' => 200.0, 'p100' => 200.0], $leg->stopSlippageBps);
        self::assertSame(20.625, $leg->stopSlippageMeanBps);
        self::assertSame(0.03, $leg->stressProbability);
        self::assertSame('taker', $leg->entry);
        self::assertNull($leg->makerFillProbability);
        self::assertNull($leg->makerTtlMinutes);
        self::assertNull($leg->measuredP95HalfSpreadBps);

        $arithmetic = $run->metrics->costArithmetic;
        self::assertNotNull($arithmetic);
        self::assertSame('15m', $arithmetic->decisionTf);
        self::assertFalse($arithmetic->protocolTf);
        self::assertSame(0.10, $arithmetic->costRLimit);
        self::assertSame(9, $arithmetic->tradesWithR);
        self::assertSame(['fees', 'slippage', 'spread'], array_keys($arithmetic->costBreakdown));
        self::assertFalse($arithmetic->costThirdPass);
        self::assertNotNull($arithmetic->roundTripCostRateMean);

        $counters = $run->metrics->costCounters;
        self::assertNotNull($counters);
        self::assertNull($counters->symbol);
        self::assertSame(4, $counters->stopFillsDrawn);
        self::assertNull($counters->makerEntryMakerShare);
        self::assertCount(2, $counters->byPair);
        self::assertSame('ETH-USD', $counters->byPair[1]->symbol);
        self::assertSame([], $counters->byPair[1]->byPair);

        self::assertCount(10, $run->trades);
        self::assertNotNull($run->trades[0]->spreadCost);
        self::assertGreaterThan(0.0, $run->trades[0]->spreadCost);
    }

    public function testReadsTheMakerRun(): void
    {
        $run = $this->reader()->read(RunFixture::PROFILES_SOURCE . '/maker');

        $costs = $run->metrics->costs;
        self::assertNotNull($costs);
        self::assertSame('maker', $costs->profile);
        self::assertFalse($costs->gatesProfile);
        self::assertSame('maker', $costs->legs[0]->entry);
        self::assertSame(0.45, $costs->legs[0]->makerFillProbability);
        self::assertSame(15, $costs->legs[0]->makerTtlMinutes);
        self::assertSame(1.5, $costs->legs[0]->adverseSelectionBps);
        self::assertSame(0.7, $run->metrics->costCounters?->makerEntryMakerShare);
        self::assertSame(7, $run->metrics->costCounters->makerEntriesFilledMaker);
        self::assertNotContains('no_maker_fill_probability', $run->metrics->simDeviations);
        self::assertSame('maker', $run->trades[0]->entryLiquidity);
    }

    public function testTheOptionalS10ManifestFieldsMayBeAbsent(): void
    {
        $copy = $this->copy();
        foreach (['profile', 'costs_file', 'costs_file_hash', 'profiles_id'] as $key) {
            $copy->unsetJson('baseline/manifest.json', [$key]);
        }

        $run = $this->reader()->read($copy->path('baseline'));

        self::assertNull($run->manifest->profile);
        self::assertNull($run->manifest->costsFileHash);
        self::assertNull($run->manifest->profilesId);
    }

    public function testAMeasuresFileIsRead(): void
    {
        $copy = $this->copy();
        $copy->setJson('baseline/metrics.json', ['costs', 'cost_measures_file'], 'measures/cost-measures-20261101.json');
        $copy->setJson('baseline/metrics.json', ['costs', 'cost_measures_hash'], 'blake3:' . str_repeat('ab', 32));
        $copy->setJson('baseline/metrics.json', ['costs', 'stress_calibrated'], true);
        $copy->setJson('baseline/metrics.json', ['costs', 'stress_calibration_ref'], 'blake3:' . str_repeat('cd', 32));

        $costs = $this->reader()->read($copy->path('baseline'))->metrics->costs;

        self::assertSame('measures/cost-measures-20261101.json', $costs?->costMeasuresFile);
        self::assertSame('blake3:' . str_repeat('ab', 32), $costs->costMeasuresHash);
        self::assertTrue($costs->stressCalibrated);
        self::assertSame('blake3:' . str_repeat('cd', 32), $costs->stressCalibrationRef);
    }

    /**
     * @return iterable<string, array{string}>
     */
    public static function mandatoryBlocks(): iterable
    {
        yield 'costs' => ['costs'];
        yield 'cost_arithmetic' => ['cost_arithmetic'];
        yield 'cost_counters' => ['cost_counters'];
    }

    #[DataProvider('mandatoryBlocks')]
    public function testEachS10BlockIsMandatory(string $block): void
    {
        $copy = $this->copy();
        $copy->unsetJson('baseline/metrics.json', [$block]);

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json : clé absente à ' . $block);
        $this->reader()->read($copy->path('baseline'));
    }

    #[DataProvider('mandatoryBlocks')]
    public function testANullS10BlockIsRefused(string $block): void
    {
        $copy = $this->copy();
        $copy->setJson('baseline/metrics.json', [$block], null);

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json : ' . $block . ' doit être un objet, trouvé null');
        $this->reader()->read($copy->path('baseline'));
    }

    public function testNestedKeysNameTheirPath(): void
    {
        $copy = $this->copy();
        $copy->unsetJson('baseline/metrics.json', ['costs', 'legs']);
        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json : clé absente à costs.legs');
        $this->reader()->read($copy->path('baseline'));
    }

    public function testAMissingStopQuantileNamesItsPath(): void
    {
        $copy = $this->copy();
        $copy->unsetJson('baseline/metrics.json', ['costs', 'legs']);
        $copy->setJson('baseline/metrics.json', ['costs', 'legs'], [[
            'symbol' => 'BTC-USD', 'class' => 'major', 'class_source' => 'list', 'spread_source' => 'default',
            'slippage_source' => 'default', 'half_spread_bps' => 0.5, 'market_slippage_bps' => 1.0,
            'stop_slippage_bps' => ['p50' => 5.0, 'p99' => 200.0, 'p100' => 200.0],
        ]]);

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json : clé absente à costs.legs[0].stop_slippage_bps.p95');
        $this->reader()->read($copy->path('baseline'));
    }

    public function testNullableArithmeticValuesAreAccepted(): void
    {
        $copy = $this->copy();
        foreach (['cost_r_mean', 'cost_r_median', 'cost_r_pass', 'gross_expectancy', 'cost_to_gross', 'gross_median_vs_own_cost_pass', 'hurdle_annual'] as $key) {
            $copy->setJson('baseline/metrics.json', ['cost_arithmetic', $key], null);
        }

        $arithmetic = $this->reader()->read($copy->path('baseline'))->metrics->costArithmetic;

        self::assertNotNull($arithmetic);
        self::assertNull($arithmetic->costRMean);
        self::assertNull($arithmetic->costRPass);
        self::assertNull($arithmetic->grossMedianVsOwnCostPass);
        self::assertNull($arithmetic->hurdleAnnual);
    }

    public function testAnArithmeticKeyThatIsAbsentIsRefused(): void
    {
        $copy = $this->copy();
        $copy->unsetJson('baseline/metrics.json', ['cost_arithmetic', 'hurdle_annual']);

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json : clé absente à cost_arithmetic.hurdle_annual');
        $this->reader()->read($copy->path('baseline'));
    }

    public function testACostBreakdownValueMustBeANumber(): void
    {
        $copy = $this->copy();
        $copy->setJson('baseline/metrics.json', ['cost_arithmetic', 'cost_breakdown', 'fees'], 'beaucoup');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json : cost_arithmetic.cost_breakdown.fees doit être un nombre, trouvé string');
        $this->reader()->read($copy->path('baseline'));
    }

    public function testTheManifestGridIsReadFromTheCostsBlockOfTheConfig(): void
    {
        $copy = $this->copy();
        $copy->unsetJson('baseline/manifest.json', ['config', 'costs', 'fee_grid', 'grid']);

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('manifest.json : clé absente à config.costs.fee_grid.grid');
        $this->reader()->read($copy->path('baseline'));
    }

    public function testAnS10ManifestWithS9MetricsIsRefused(): void
    {
        $copy = $this->copy();
        $copy->replace('baseline/metrics.json', '"tv4-metrics-s10"', '"tv4-metrics-s9"');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json : schema : "tv4-metrics-s9" ne correspond pas au schéma du manifeste "tv4-manifest-s10"');
        $this->reader()->read($copy->path('baseline'));
    }

    public function testAnS9ManifestWithS10MetricsIsRefused(): void
    {
        $copy = RunFixture::copyPortfolio();
        $this->copy = $copy;
        $copy->replace('metrics.json', '"tv4-metrics-s9"', '"tv4-metrics-s10"');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json : schema : "tv4-metrics-s10" ne correspond pas au schéma du manifeste "tv4-manifest-s9"');
        $this->reader()->read($copy->directory);
    }

    public function testAnS8ManifestWithS10MetricsIsRefused(): void
    {
        $copy = RunFixture::copy();
        $this->copy = $copy;
        $copy->replace('metrics.json', '"tv4-metrics-s8"', '"tv4-metrics-s10"');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('ne correspond pas au schéma du manifeste "tv4-manifest-s8"');
        $this->reader()->read($copy->directory);
    }

    public function testTheProfileOfTheMetricsMustBeThatOfTheManifest(): void
    {
        $copy = $this->copy();
        $copy->setJson('baseline/manifest.json', ['profile'], 'adverse');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json : costs.profile : différent de celui du manifeste');
        $this->reader()->read($copy->path('baseline'));
    }

    public function testTheCostsFileHashOfTheMetricsMustBeThatOfTheManifest(): void
    {
        $copy = $this->copy();
        $copy->setJson('baseline/manifest.json', ['costs_file_hash'], 'blake3:' . str_repeat('00', 32));

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json : costs.costs_file_hash : différent de celui du manifeste');
        $this->reader()->read($copy->path('baseline'));
    }

    public function testS8AndS9RunsHaveNoCostBlocks(): void
    {
        foreach ([RunFixture::SOURCE, RunFixture::PORTFOLIO_SOURCE] as $source) {
            $run = $this->reader()->read($source);

            self::assertFalse($run->metrics->isCosts());
            self::assertNull($run->metrics->costs);
            self::assertNull($run->metrics->costArithmetic);
            self::assertNull($run->metrics->costCounters);
            self::assertNull($run->manifest->profile);
            self::assertNotNull($run->manifest->config->slippageBps);
        }
    }
}
