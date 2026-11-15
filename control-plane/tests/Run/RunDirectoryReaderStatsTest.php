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

final class RunDirectoryReaderStatsTest extends TestCase
{
    public const SOURCE = __DIR__ . '/../fixtures/stats-rust/simple';

    private function reader(): RunDirectoryReader
    {
        return new RunDirectoryReader(new TradesCsvReader(), new EquityCsvReader());
    }

    public function testRealRustOutputWithMaximumSeedAndNegativeRIsRead(): void
    {
        $report = $this->reader()->read(self::SOURCE);
        self::assertNotNull($report->stats);
        self::assertSame('18446744073709551615', $report->stats->data['options']['seed']);
        self::assertSame(43, $report->stats->data['bootstrap']['n_trades']);
        self::assertLessThan(0, $report->stats->data['bootstrap']['mean_r']['value']);
    }

    public function testOldRunWithoutSidecarStillReads(): void
    {
        self::assertNull($this->reader()->read(RunFixture::SOURCE)->stats);
    }

    /** @return iterable<string, array{string, mixed}> */
    public static function invalidValues(): iterable
    {
        yield 'schema' => ['schema', 'tv4-stats-v2'];
        foreach ([42, 1.5, '01', '-1', '18446744073709551616', '1e2', ''] as $seed) {
            yield 'seed ' . var_export($seed, true) => ['options.seed', $seed];
        }
        yield 'samples' => ['options.samples', 9999];
        yield 'block' => ['options.bootstrap_block', 11];
        yield 'correlation' => ['options.correlation_threshold', 1];
        yield 'source id' => ['source.provenance.run_id', str_repeat('a', 64)];
        yield 'source capital' => ['source.provenance.initial_cash', 301];
        yield 'source window' => ['source.provenance.window.start_ms', 1704085260000];
        yield 'source profile' => ['source.provenance.profile', 'baseline'];
        yield 'source line' => ['source.provenance.line', 'full'];
        yield 'source outputs' => ['source.files.equity_csv', 'blake3:' . str_repeat('a', 64)];
        yield 'source packet' => ['source.files.stats_inputs_json', null];
        yield 'history' => ['source.historical_governance.read_id', 999];
        yield 'unknown partition' => ['partition', 'DEV'];
        yield 'metric contradiction' => ['bootstrap.mean_r.availability', 'unavailable'];
        yield 'metric missing reason' => ['concentration.top3_share.reasons', []];
        yield 'negative standard error' => ['bootstrap.standard_error_r.value', -0.1];
        yield 'n r' => ['bootstrap.n_r', 44];
        yield 'blocks cardinality' => ['monte_carlo.n_blocks', 3];
        yield 'peak dd bound' => ['monte_carlo.dd_peak_p95_2y.value', 1.1];
        yield 'stop probability' => ['monte_carlo.probability_dd20_1y.value', -0.01];
        yield 'return bound' => ['monte_carlo.return_p5_2y.value', -1.1];
        yield 'regime cells' => ['regimes.cells', []];
        yield 'non displayed trend' => ['regimes.labels.0.trend', 'sideways'];
        yield 'non displayed daily' => ['benchmarks.strategy_daily.intervals.0.value', -1.1];
        yield 'registry cardinality' => ['registry.value.declared_n', 6];
        yield 'corpus coverage' => ['corpus.coverage', 2];
        yield 'false global estimation' => ['corpus.clustering.value.global_n_eff', ['availability' => 'complete', 'value' => 1, 'reasons' => []]];
        yield 'false current n_eff' => ['current_noise.n_eff', 1];
        yield 'false daily complete count' => ['corpus.complete_daily_configs', 0];
        yield 'duplicate series date' => ['benchmarks.strategy_daily.intervals.1.start_ms', 1704153600000];
        yield 'false cells n' => ['regimes.cells.0.n', 1];
        yield 'false mc removal count' => ['monte_carlo.n_removed', 7];
        yield 'false concentration availability' => ['concentration.availability', 'complete'];
        yield 'daily coverage contradiction' => ['benchmarks.strategy_daily.coverage', 0.25];
        yield 'corpus coverage contradiction' => ['corpus.coverage', 0.5];
        yield 'clustering coverage contradiction' => ['corpus.clustering.value.coverage', 0.5];
        yield 'future regime source' => ['regimes.labels.0.close_ms', 1704219360001];
        yield 'false bootstrap availability' => ['bootstrap.availability', 'partial'];
        yield 'false mc unavailable' => ['monte_carlo.availability', 'unavailable'];
        yield 'false regimes missing labels' => ['regimes.missing_labels', 1];
        yield 'false sizing' => ['monte_carlo.sizing', 'research'];
        yield 'unknown field' => ['options.unexpected', 1];
    }

    #[DataProvider('invalidValues')]
    public function testRejectsInvalidOrIncoherentSidecar(string $path, mixed $value): void
    {
        $copy = RunFixture::copy(self::SOURCE);
        try {
            $copy->write('stats.json', (string) file_get_contents(self::SOURCE . '/stats.json'));
            $copy->setJson('stats.json', explode('.', $path), $value);
            $this->expectException(InvalidRunException::class);
            $this->expectExceptionMessage('stats.json');
            $this->reader()->read($copy->directory);
        } finally {
            $copy->cleanup();
        }
    }

    public function testRejectsOverflowEvenInUndisplayedField(): void
    {
        $copy = RunFixture::copy(self::SOURCE);
        try {
            $json = (string) file_get_contents(self::SOURCE . '/stats.json');
            $copy->write('stats.json', str_replace('"selection_years": 5.0', '"selection_years": 1e400', $json));
            $this->expectException(InvalidRunException::class);
            $this->reader()->read($copy->directory);
        } finally {
            $copy->cleanup();
        }
    }

    public function testInitialCapitalDrawdownMayExceedOne(): void
    {
        $copy = RunFixture::copy(self::SOURCE);
        try {
            $copy->write('stats.json', (string) file_get_contents(self::SOURCE . '/stats.json'));
            $copy->setJson('stats.json', ['monte_carlo', 'dd_initial_p95_2y', 'value'], 2);
            self::assertNotNull($this->reader()->read($copy->directory)->stats);
        } finally {
            $copy->cleanup();
        }
    }

    public function testCapitalAllowsBoundedSerializationDifference(): void
    {
        $copy = RunFixture::copy(self::SOURCE);
        try {
            $copy->write('stats.json', (string) file_get_contents(self::SOURCE . '/stats.json'));
            $copy->setJson('stats.json', ['source', 'provenance', 'initial_cash'], 300.00000000000006);
            self::assertNotNull($this->reader()->read($copy->directory)->stats);
        } finally {
            $copy->cleanup();
        }
    }

    public function testExternalInputsHashWithoutLocalPacketReferenceIsValid(): void
    {
        $copy = RunFixture::copy(self::SOURCE);
        try {
            $copy->write('stats.json', (string) file_get_contents(self::SOURCE . '/stats.json'));
            $copy->unsetJson('manifest.json', ['stats_inputs']);
            self::assertNotNull($this->reader()->read($copy->directory)->stats);
        } finally {
            $copy->cleanup();
        }
    }

    public function testEmptyJsonArrayCannotReplaceObject(): void
    {
        $copy = RunFixture::copy(self::SOURCE);
        try {
            $copy->write('stats.json', (string) file_get_contents(self::SOURCE . '/stats.json'));
            $copy->setJson('stats.json', ['source', 'files'], []);
            $this->expectException(InvalidRunException::class);
            $this->reader()->read($copy->directory);
        } finally {
            $copy->cleanup();
        }
    }

    /** @return iterable<string, array{bool}> */
    public static function unverifiableRegistries(): iterable
    {
        yield 'registre étranger et panel inconnu' => [false];
        yield 'registre vide' => [true];
    }

    #[DataProvider('unverifiableRegistries')]
    public function testReadableRegistryDoesNotImplyVerifiedGlobalN(bool $empty): void
    {
        $copy = RunFixture::copy(self::SOURCE);
        try {
            $copy->write('stats.json', (string) file_get_contents(self::SOURCE . '/stats.json'));
            if ($empty) {
                $copy->setJson('stats.json', ['registry', 'value', 'declared_n'], 0);
                $copy->setJson('stats.json', ['registry', 'value', 'last_event_id'], null);
                $copy->setJson('stats.json', ['registry', 'value', 'registered_hashes'], []);
            } else {
                $copy->setJson('stats.json', ['registry', 'value', 'registered_hashes', '2'], str_repeat('a', 64));
            }
            $copy->setJson('stats.json', ['corpus', 'members', '0', 'registry_member'], false);
            $copy->setJson('stats.json', ['corpus', 'declared_verified_n'], null);
            $copy->setJson('stats.json', ['corpus', 'coverage'], null);
            $copy->setJson('stats.json', ['corpus', 'clustering', 'value', 'declared_verified_n'], null);
            $copy->setJson('stats.json', ['corpus', 'clustering', 'value', 'coverage'], null);
            $copy->setJson('stats.json', ['current_noise'], null);
            $report = $this->reader()->read($copy->directory);
            self::assertNotNull($report->stats);
            self::assertSame('complete', $report->stats->data['registry']['availability']);
            self::assertNull($report->stats->data['corpus']['declared_verified_n']);
            self::assertNull($report->stats->data['current_noise']);
        } finally {
            $copy->cleanup();
        }
    }

    public function testUnknownGlobalNDoesNotPublishNoiseFromTheReadableRegistry(): void
    {
        $copy = RunFixture::copy(self::SOURCE);
        try {
            $copy->write('stats.json', (string) file_get_contents(self::SOURCE . '/stats.json'));
            $copy->setJson('stats.json', ['registry', 'value', 'registered_hashes', '2'], str_repeat('a', 64));
            $copy->setJson('stats.json', ['corpus', 'members', '0', 'registry_member'], false);
            $copy->setJson('stats.json', ['corpus', 'declared_verified_n'], null);
            $copy->setJson('stats.json', ['corpus', 'coverage'], null);
            $copy->setJson('stats.json', ['corpus', 'clustering', 'value', 'declared_verified_n'], null);
            $copy->setJson('stats.json', ['corpus', 'clustering', 'value', 'coverage'], null);
            $this->expectException(InvalidRunException::class);
            $this->reader()->read($copy->directory);
        } finally {
            $copy->cleanup();
        }
    }
}
