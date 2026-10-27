<?php

declare(strict_types=1);

namespace App\Run;

use App\Run\Csv\EquityCsvReader;
use App\Run\Csv\TradesCsvReader;
use App\Run\Dto\ConfigInfo;
use App\Run\Dto\Distribution;
use App\Run\Dto\EngineInfo;
use App\Run\Dto\ExitTypeStat;
use App\Run\Dto\Exposure;
use App\Run\Dto\Funnel;
use App\Run\Dto\Histogram;
use App\Run\Dto\Manifest;
use App\Run\Dto\ManifestData;
use App\Run\Dto\MaxDrawdown;
use App\Run\Dto\Metrics;
use App\Run\Dto\PnlWithoutBest;
use App\Run\Dto\RunReport;
use App\Run\Dto\RunWindow;
use App\Run\Dto\Timings;
use App\Run\Dto\YearStat;
use App\Run\Json\JsonObject;

/**
 * Lit les quatre fichiers d'un run (`manifest.json`, `metrics.json`, `trades.csv`, `equity.csv`) et
 * les valide. Aucun hash ni aucune métrique n'est recalculé : `tv4` reste la source de vérité.
 */
final class RunDirectoryReader
{
    public const MANIFEST_FILE = 'manifest.json';
    public const METRICS_FILE = 'metrics.json';
    public const MANIFEST_SCHEMA = 'tv4-manifest-s8';
    public const METRICS_SCHEMA = 'tv4-metrics-s8';

    public function __construct(
        private readonly TradesCsvReader $tradesReader,
        private readonly EquityCsvReader $equityReader,
    ) {
    }

    /**
     * @throws InvalidRunException
     */
    public function read(string $directory): RunReport
    {
        if (!is_dir($directory)) {
            throw InvalidRunException::directoryNotFound($directory);
        }
        $dir = rtrim($directory, '/\\');
        foreach ([self::MANIFEST_FILE, self::METRICS_FILE, TradesCsvReader::FILE, EquityCsvReader::FILE] as $file) {
            if (!is_file($dir . '/' . $file)) {
                throw InvalidRunException::missingFile($file);
            }
        }

        $manifest = $this->manifest($this->json($dir, self::MANIFEST_FILE, self::MANIFEST_SCHEMA));
        $metrics = $this->metrics($this->json($dir, self::METRICS_FILE, self::METRICS_SCHEMA));

        if ($manifest->configHash !== $metrics->configHash) {
            throw InvalidRunException::invalidValue(self::METRICS_FILE, 'config_hash', 'différent de celui du manifeste');
        }
        if ($manifest->datasetHash !== $metrics->datasetHash) {
            throw InvalidRunException::invalidValue(self::METRICS_FILE, 'dataset_hash', 'différent de celui du manifeste');
        }

        return new RunReport(
            $manifest,
            $metrics,
            $this->tradesReader->read($dir . '/' . TradesCsvReader::FILE),
            $this->equityReader->read($dir . '/' . EquityCsvReader::FILE),
        );
    }

    private function json(string $dir, string $file, string $schema): JsonObject
    {
        $content = @file_get_contents($dir . '/' . $file);
        if (false === $content) {
            throw InvalidRunException::unreadableFile($file);
        }
        $json = JsonObject::decode($content, $file);
        if (!$json->has('schema')) {
            throw InvalidRunException::missingKey($file, 'schema');
        }
        $found = $json->string('schema');
        if ($found !== $schema) {
            throw InvalidRunException::unknownSchema($file, $found, $schema);
        }

        return $json;
    }

    private function manifest(JsonObject $j): Manifest
    {
        $data = $j->object('data');
        $window = $j->object('window');
        $engine = $j->object('engine');
        $timings = $j->object('timings');

        $fileHashes = [];
        foreach ($j->object('outputs')->scalarMap('files') as $name => $hash) {
            if (!is_string($hash)) {
                throw InvalidRunException::wrongType(self::MANIFEST_FILE, 'outputs.files.' . $name, 'une chaîne', get_debug_type($hash));
            }
            $fileHashes[$name] = $hash;
        }

        return new Manifest(
            runId: $j->string('run_id'),
            configHash: $j->string('config_hash'),
            datasetHash: $j->string('dataset_hash'),
            hashFormat: $j->string('hash_format'),
            resultHash: $j->string('result_hash'),
            verdict: $j->string('verdict'),
            config: $this->config($j->object('config')),
            data: new ManifestData(
                source: $data->string('source'),
                dataset: $data->nullableString('dataset'),
                fixtureBlake3: $data->nullableString('fixture_blake3'),
                exchange: $data->string('exchange'),
                symbol: $data->string('symbol'),
                tf: $data->string('tf'),
                gapsPolicy: $data->string('gaps_policy'),
                bars: $data->int('bars'),
                syntheticBars: $data->int('synthetic_bars'),
                firstOpenTimeMs: $data->int('first_open_time_ms'),
                lastOpenTimeMs: $data->int('last_open_time_ms'),
            ),
            window: new RunWindow(
                start: $window->string('start'),
                end: $window->string('end'),
                startMs: $window->int('start_ms'),
                endMs: $window->int('end_ms'),
            ),
            engine: new EngineInfo(
                tv4Version: $engine->string('tv4_version'),
                gitSha: $engine->string('git_sha'),
                rustc: $engine->string('rustc'),
                rustcCommit: $engine->string('rustc_commit'),
                target: $engine->string('target'),
                targetCpu: $engine->string('target_cpu'),
                profile: $engine->string('profile'),
                binaryHash: $engine->string('binary_hash'),
            ),
            equityStep: $j->string('outputs.equity_step'),
            fileHashes: $fileHashes,
            startedAt: $j->string('started_at'),
            timings: new Timings(
                loadMs: $timings->int('load_ms'),
                runMs: $timings->int('run_ms'),
                cliColdMs: $timings->int('cli_cold_ms'),
            ),
        );
    }

    private function config(JsonObject $c): ConfigInfo
    {
        return new ConfigInfo(
            strategyId: $c->string('strategy.id'),
            strategyVersion: $c->string('strategy.version'),
            strategyParams: $c->object('strategy')->scalarMap('params'),
            feesExchange: $c->string('fees.exchange'),
            makerBps: $c->float('fees.maker_bps'),
            takerBps: $c->float('fees.taker_bps'),
            slippageBps: $c->float('slippage_bps'),
            initialCash: $c->float('initial_cash'),
            minTrades: $c->int('min_trades'),
            warmupBars: $c->int('warmup_bars'),
            tv4Version: $c->string('tv4_version'),
            simRules: $c->scalarMap('sim_rules'),
            instrument: $c->scalarMap('instrument'),
        );
    }

    private function metrics(JsonObject $j): Metrics
    {
        $signals = $j->object('signals');

        return new Metrics(
            configHash: $j->string('config_hash'),
            datasetHash: $j->string('dataset_hash'),
            dataBars: $j->int('data.bars'),
            dataSyntheticBars: $j->int('data.synthetic_bars'),
            minutesMissing: $j->int('data.minutes_missing'),
            windowStartIndex: $j->int('window.start_index'),
            windowBars: $j->int('window.bars'),
            nTrades: $j->int('n_trades'),
            nTradesForced: $j->int('n_trades_forced'),
            forcedClose: $j->bool('forced_close'),
            pnlNet: $j->float('pnl_net'),
            pnlRealized: $j->float('pnl_realized'),
            pnlMarked: $j->float('pnl_marked'),
            feesTotal: $j->float('fees_total'),
            slippageTotal: $j->float('slippage_total'),
            equityFinal: $j->float('equity_final'),
            cashFinal: $j->float('cash_final'),
            returnTotal: $j->float('return_total'),
            fills: $j->int('fills'),
            sim: $j->intMap('sim'),
            signalsEmitted: $signals->int('emitted'),
            signalsAccepted: $signals->int('accepted'),
            signalsRejected: $signals->int('rejected'),
            rejectedByReason: $signals->intMap('rejected_by_reason'),
            simDeviations: $j->stringList('sim_deviations'),
            minTrades: $j->int('min_trades'),
            verdict: $j->string('verdict'),
            expectancyNet: $j->nullableFloat('expectancy_net'),
            expectancyR: $j->nullableFloat('expectancy_r'),
            tradesWithR: $j->int('trades_with_r'),
            nWins: $j->int('n_wins'),
            nLosses: $j->int('n_losses'),
            winRate: $j->nullableFloat('win_rate'),
            grossProfit: $j->float('gross_profit'),
            grossLoss: $j->float('gross_loss'),
            profitFactor: $j->nullableFloat('profit_factor'),
            avgHoldingMs: $j->nullableFloat('avg_holding_ms'),
            mfeMean: $j->nullableFloat('mfe_mean'),
            maeMean: $j->nullableFloat('mae_mean'),
            drawdownM1: $this->drawdown($j->object('max_drawdown.m1')),
            drawdownH1: $this->drawdown($j->object('max_drawdown.h1')),
            exposure: new Exposure(
                barsInPosition: $j->int('exposure.bars_in_position'),
                barsInWindow: $j->int('exposure.bars_in_window'),
                fraction: $j->float('exposure.fraction'),
            ),
            funnel: $this->funnel($j->object('funnel')),
            distribution: $this->distribution($j->object('distribution')),
        );
    }

    private function drawdown(JsonObject $d): MaxDrawdown
    {
        return new MaxDrawdown(
            pct: $d->float('pct'),
            quote: $d->float('quote'),
            peakMs: $d->nullableInt('peak_ms'),
            troughMs: $d->nullableInt('trough_ms'),
        );
    }

    private function funnel(JsonObject $f): Funnel
    {
        return new Funnel(
            evaluations: $f->int('evaluations'),
            rawEntrySignals: $f->nullableInt('raw_entry_signals'),
            entryFilteredByReason: $f->intMap('entry_filtered_by_reason'),
            entrySignals: $f->int('entry_signals'),
            entryRejectedByReason: $f->intMap('entry_rejected_by_reason'),
            entryOrders: $f->int('entry_orders'),
            entryNotFilledByReason: $f->intMap('entry_not_filled_by_reason'),
            entryFills: $f->int('entry_fills'),
            trades: $f->int('trades'),
            exitSignals: $f->int('exit_signals'),
            exitOrders: $f->int('exit_orders'),
            otherSignals: $f->int('other_signals'),
            otherAccepted: $f->int('other_accepted'),
            exitsByType: $f->intMap('exits_by_type'),
        );
    }

    private function distribution(JsonObject $d): Distribution
    {
        $histogram = null;
        if (!$d->isNull('histogram_r')) {
            $h = $d->object('histogram_r');
            $histogram = new Histogram(
                binWidth: $h->float('bin_width'),
                min: $h->float('min'),
                max: $h->float('max'),
                counts: $h->intList('counts'),
                underflow: $h->int('underflow'),
                overflow: $h->int('overflow'),
            );
        }

        return new Distribution(
            pnlNetMean: $d->nullableFloat('pnl_net_mean'),
            pnlNetMedian: $d->nullableFloat('pnl_net_median'),
            rMean: $d->nullableFloat('r_mean'),
            rMedian: $d->nullableFloat('r_median'),
            histogramR: $histogram,
            pnlWithoutBest: array_map(
                static fn (JsonObject $o): PnlWithoutBest => new PnlWithoutBest($o->int('n'), $o->nullableFloat('pnl_net')),
                $d->objectList('pnl_without_best'),
            ),
            byYear: array_map(
                static fn (JsonObject $o): YearStat => new YearStat(
                    $o->int('year'),
                    $o->int('n_trades'),
                    $o->int('n_wins'),
                    $o->float('pnl_net'),
                ),
                $d->objectList('by_year'),
            ),
            byExitType: array_map(
                static fn (JsonObject $o): ExitTypeStat => new ExitTypeStat(
                    $o->string('exit_type'),
                    $o->int('n_trades'),
                    $o->float('pnl_net'),
                ),
                $d->objectList('by_exit_type'),
            ),
        );
    }
}
