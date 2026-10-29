<?php

declare(strict_types=1);

namespace App\Run;

use App\Run\Csv\EquityCsvReader;
use App\Run\Csv\TradesCsvReader;
use App\Run\Dto\ConfigInfo;
use App\Run\Dto\CostArithmetic;
use App\Run\Dto\CostCounters;
use App\Run\Dto\CostLeg;
use App\Run\Dto\CostsBlock;
use App\Run\Dto\DataSeries;
use App\Run\Dto\Distribution;
use App\Run\Dto\EngineInfo;
use App\Run\Dto\Executability;
use App\Run\Dto\ExecutabilityPair;
use App\Run\Dto\ExecutabilityRejection;
use App\Run\Dto\ExitTypeStat;
use App\Run\Dto\Exposure;
use App\Run\Dto\Funnel;
use App\Run\Dto\G1Executable;
use App\Run\Dto\Histogram;
use App\Run\Dto\InstrumentsInfo;
use App\Run\Dto\Manifest;
use App\Run\Dto\ManifestData;
use App\Run\Dto\MaxDrawdown;
use App\Run\Dto\Metrics;
use App\Run\Dto\PairFunnel;
use App\Run\Dto\PairStat;
use App\Run\Dto\PnlWithoutBest;
use App\Run\Dto\PortfolioInfo;
use App\Run\Dto\PriorityInfo;
use App\Run\Dto\RegimeInfo;
use App\Run\Dto\RunReport;
use App\Run\Dto\RunWindow;
use App\Run\Dto\Timings;
use App\Run\Dto\UpcomingChange;
use App\Run\Dto\YearStat;
use App\Run\Json\JsonObject;

/**
 * Lit les quatre fichiers d'un run (`manifest.json`, `metrics.json`, `trades.csv`, `equity.csv`) et
 * les valide. Aucun hash ni aucune métrique n'est recalculé : `tv4` reste la source de vérité.
 *
 * Trois générations de schémas sont acceptées, chacune en comparaison stricte de valeur : s8 (une paire),
 * s9 (portefeuille) et s10 (portefeuille joué avec un profil de coûts). Le manifeste et les métriques d'un même
 * run doivent être de la même génération. Les blocs propres à s9 sont lus s'ils sont présents ; un run s8 se
 * lit exactement comme avant. En s10, les blocs `costs`, `cost_arithmetic` et `cost_counters` sont obligatoires,
 * `fees` et `slippage_bps` n'existent plus et la grille de frais se lit dans `costs.fee_grid` de la config.
 */
final class RunDirectoryReader
{
    public const MANIFEST_FILE = 'manifest.json';
    public const METRICS_FILE = 'metrics.json';
    public const MANIFEST_SCHEMA_S8 = 'tv4-manifest-s8';
    public const MANIFEST_SCHEMA_S9 = 'tv4-manifest-s9';
    public const METRICS_SCHEMA_S8 = 'tv4-metrics-s8';
    public const METRICS_SCHEMA_S9 = 'tv4-metrics-s9';
    public const MANIFEST_SCHEMA_S10 = 'tv4-manifest-s10';
    public const METRICS_SCHEMA_S10 = 'tv4-metrics-s10';

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

        $manifestSchemas = [self::MANIFEST_SCHEMA_S8, self::MANIFEST_SCHEMA_S9, self::MANIFEST_SCHEMA_S10];
        $metricsSchemas = [self::METRICS_SCHEMA_S8, self::METRICS_SCHEMA_S9, self::METRICS_SCHEMA_S10];
        $manifestJson = $this->json($dir, self::MANIFEST_FILE, $manifestSchemas);
        $metricsJson = $this->json($dir, self::METRICS_FILE, $metricsSchemas);
        $generation = self::generation($manifestJson->string('schema'));
        $costs = 10 === $generation;
        $portfolio = $generation >= 9;
        if ($generation !== self::generation($metricsJson->string('schema'))) {
            throw InvalidRunException::invalidValue(
                self::METRICS_FILE,
                'schema',
                sprintf(
                    '"%s" ne correspond pas au schéma du manifeste "%s"',
                    $metricsJson->string('schema'),
                    $manifestJson->string('schema'),
                ),
            );
        }

        $manifest = $this->manifest($manifestJson, $portfolio, $costs);
        $metrics = $this->metrics($metricsJson, $portfolio, $costs);

        if ($manifest->configHash !== $metrics->configHash) {
            throw InvalidRunException::invalidValue(self::METRICS_FILE, 'config_hash', 'différent de celui du manifeste');
        }
        if ($manifest->datasetHash !== $metrics->datasetHash) {
            throw InvalidRunException::invalidValue(self::METRICS_FILE, 'dataset_hash', 'différent de celui du manifeste');
        }
        $instrumentsHash = $metrics->instruments?->instrumentsHash;
        if (null !== $manifest->instrumentsHash && null !== $instrumentsHash && $manifest->instrumentsHash !== $instrumentsHash) {
            throw InvalidRunException::invalidValue(self::METRICS_FILE, 'instruments.instruments_hash', 'différent de celui du manifeste');
        }

        $this->checkCostsConsistency($manifest, $metrics);

        return new RunReport(
            $manifest,
            $metrics,
            $this->tradesReader->read($dir . '/' . TradesCsvReader::FILE),
            $this->equityReader->read($dir . '/' . EquityCsvReader::FILE),
        );
    }

    /** Génération d'un nom de schéma accepté : 8, 9 ou 10 (suffixe `-s8`, `-s9`, `-s10`). */
    private static function generation(string $schema): int
    {
        return match (true) {
            str_ends_with($schema, '-s10') => 10,
            str_ends_with($schema, '-s9') => 9,
            default => 8,
        };
    }

    /** En s10, le manifeste et les métriques du même run annoncent le même profil et le même fichier de coûts. */
    private function checkCostsConsistency(Manifest $manifest, Metrics $metrics): void
    {
        $costs = $metrics->costs;
        if (null === $costs) {
            return;
        }
        if (null !== $manifest->profile && $manifest->profile !== $costs->profile) {
            throw InvalidRunException::invalidValue(self::METRICS_FILE, 'costs.profile', 'différent de celui du manifeste');
        }
        if (null !== $manifest->costsFileHash && $manifest->costsFileHash !== $costs->costsFileHash) {
            throw InvalidRunException::invalidValue(self::METRICS_FILE, 'costs.costs_file_hash', 'différent de celui du manifeste');
        }
    }

    /**
     * @param list<string> $schemas schémas acceptés
     */
    private function json(string $dir, string $file, array $schemas): JsonObject
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
        if (!in_array($found, $schemas, true)) {
            throw InvalidRunException::unknownSchema($file, $found, $schemas);
        }

        return $json;
    }

    private function manifest(JsonObject $j, bool $portfolio, bool $costs): Manifest
    {
        $data = $j->object('data');
        // s9 : le bloc `data` reprend la fenêtre, le hash et son format ; le haut du manifeste reste prioritaire.
        $window = $portfolio && !$j->has('window') ? $data->object('window') : $j->object('window');
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
            datasetHash: $portfolio && !$j->has('dataset_hash') ? $data->string('dataset_hash') : $j->string('dataset_hash'),
            hashFormat: $portfolio && !$j->has('hash_format') ? $data->string('hash_format') : $j->string('hash_format'),
            resultHash: $j->string('result_hash'),
            verdict: $j->string('verdict'),
            config: $this->config($j->object('config'), $portfolio, $costs),
            data: $portfolio ? $this->portfolioData($data) : new ManifestData(
                source: $data->string('source'),
                dataset: $data->nullableString('dataset'),
                fixtureBlake3: $data->nullableString('fixture_blake3'),
                exchange: $data->string('exchange'),
                symbols: [$data->string('symbol')],
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
            schema: $j->string('schema'),
            instrumentsHash: $portfolio && $j->has('instruments_hash') ? $j->nullableString('instruments_hash') : null,
            instrumentsFile: $portfolio && $j->has('instruments_file') ? $j->nullableString('instruments_file') : null,
            profile: $costs ? $this->optionalString($j, 'profile') : null,
            costsFile: $costs ? $this->optionalString($j, 'costs_file') : null,
            costsFileHash: $costs ? $this->optionalString($j, 'costs_file_hash') : null,
            costMeasuresFile: $costs ? $this->optionalString($j, 'cost_measures_file') : null,
            costMeasuresHash: $costs ? $this->optionalString($j, 'cost_measures_hash') : null,
            profilesId: $costs ? $this->optionalString($j, 'profiles_id') : null,
        );
    }

    /** `manifest.data` d'un run s9 : univers, paire de régime et segments chargés ; le reste est facultatif. */
    private function portfolioData(JsonObject $data): ManifestData
    {
        $series = [];
        if ($data->has('series')) {
            foreach ($data->objectList('series') as $o) {
                $series[] = new DataSeries(
                    symbol: $o->string('symbol'),
                    rows: $o->int('rows'),
                    firstMs: $o->has('first_ms') ? $o->nullableInt('first_ms') : null,
                    lastMs: $o->has('last_ms') ? $o->nullableInt('last_ms') : null,
                );
            }
        }

        return new ManifestData(
            source: $this->optionalString($data, 'source'),
            dataset: $this->optionalString($data, 'dataset'),
            fixtureBlake3: $this->optionalString($data, 'fixture_blake3'),
            exchange: $data->string('exchange'),
            symbols: $data->stringList('symbols'),
            tf: $data->string('tf'),
            gapsPolicy: $data->string('gaps_policy'),
            bars: $this->optionalInt($data, 'bars'),
            syntheticBars: $this->optionalInt($data, 'synthetic_bars'),
            firstOpenTimeMs: $this->optionalInt($data, 'first_open_time_ms'),
            lastOpenTimeMs: $this->optionalInt($data, 'last_open_time_ms'),
            regimeSymbol: $this->optionalString($data, 'regime_symbol'),
            series: $series,
        );
    }

    private function optionalString(JsonObject $o, string $path): ?string
    {
        return $o->has($path) ? $o->nullableString($path) : null;
    }

    private function optionalInt(JsonObject $o, string $path): ?int
    {
        return $o->has($path) ? $o->nullableInt($path) : null;
    }

    private function optionalFloat(JsonObject $o, string $path): ?float
    {
        return $o->has($path) ? $o->nullableFloat($path) : null;
    }

    /** Nombre ou chaîne numérique, null si la clé manque ou vaut null. */
    private function optionalNumber(JsonObject $o, string $path): ?float
    {
        return $o->has($path) ? $o->nullableNumber($path) : null;
    }

    /** Bloc objet facultatif : présent et non nul. */
    private function hasBlock(JsonObject $j, string $path): bool
    {
        return $j->has($path) && !$j->isNull($path);
    }

    /** En s10, la grille se lit dans `costs.fee_grid` et le slippage scalaire n'existe plus. */
    private function config(JsonObject $c, bool $portfolio, bool $costs): ConfigInfo
    {
        $fees = $costs ? 'costs.fee_grid' : 'fees';

        return new ConfigInfo(
            strategyId: $c->string('strategy.id'),
            strategyVersion: $c->string('strategy.version'),
            strategyParams: $c->object('strategy')->scalarMap('params'),
            feesExchange: $c->string($costs ? 'costs.fee_grid.grid' : 'fees.exchange'),
            makerBps: $c->float($fees . '.maker_bps'),
            takerBps: $c->float($fees . '.taker_bps'),
            slippageBps: $costs ? null : $c->float('slippage_bps'),
            initialCash: $c->float('initial_cash'),
            minTrades: $c->int('min_trades'),
            warmupBars: $c->int('warmup_bars'),
            tv4Version: $c->string('tv4_version'),
            simRules: $c->scalarMap('sim_rules'),
            instrument: $portfolio ? null : $c->scalarMap('instrument'),
            instruments: $portfolio && $this->hasBlock($c, 'instruments') ? $this->instrumentRules($c) : null,
            portfolio: $portfolio && $this->hasBlock($c, 'portfolio') ? $c->scalarMap('portfolio') : null,
        );
    }

    /**
     * @return array<string, array<string, bool|int|float|string|null>>
     */
    private function instrumentRules(JsonObject $c): array
    {
        $rules = [];
        foreach ($c->keys('instruments') as $symbol) {
            $rules[$symbol] = $c->scalarMap('instruments.' . $symbol);
        }

        return $rules;
    }

    private function metrics(JsonObject $j, bool $portfolio, bool $costs): Metrics
    {
        $signals = $j->object('signals');

        return new Metrics(
            configHash: $j->string('config_hash'),
            datasetHash: $portfolio && !$j->has('dataset_hash') ? $j->string('data.dataset_hash') : $j->string('dataset_hash'),
            dataBars: $portfolio ? $this->optionalInt($j, 'data.bars') : $j->int('data.bars'),
            dataSyntheticBars: $portfolio ? $this->optionalInt($j, 'data.synthetic_bars') : $j->int('data.synthetic_bars'),
            minutesMissing: $portfolio ? $this->optionalInt($j, 'data.minutes_missing') : $j->int('data.minutes_missing'),
            windowStartIndex: $portfolio ? $this->optionalInt($j, 'window.start_index') : $j->int('window.start_index'),
            windowBars: $portfolio ? $this->optionalInt($j, 'window.bars') : $j->int('window.bars'),
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
            funnel: $this->funnel($j->object('funnel'), $portfolio),
            distribution: $this->distribution($j->object('distribution'), $portfolio),
            schema: $j->string('schema'),
            funnelByPair: $portfolio && $this->hasBlock($j, 'funnel_by_pair') ? $this->funnelByPair($j) : null,
            portfolio: $portfolio && $this->hasBlock($j, 'portfolio') ? $this->portfolioInfo($j->object('portfolio')) : null,
            priority: $portfolio && $this->hasBlock($j, 'priority') ? $this->priority($j->object('priority')) : null,
            regime: $portfolio && $this->hasBlock($j, 'regime') ? $this->regime($j->object('regime')) : null,
            instruments: $portfolio && $this->hasBlock($j, 'instruments') ? $this->instruments($j->object('instruments')) : null,
            executability: $portfolio && $this->hasBlock($j, 'executability') ? $this->executability($j->object('executability')) : null,
            g1Executable: $portfolio && $this->hasBlock($j, 'g1_executable') ? $this->g1($j->object('g1_executable')) : null,
            signalsRejectedMaxPositions: $portfolio ? $signals->int('rejected_max_positions') : null,
            costs: $costs ? $this->costs($j->object('costs')) : null,
            costArithmetic: $costs ? $this->costArithmetic($j->object('cost_arithmetic')) : null,
            costCounters: $costs ? $this->costCounters($j->object('cost_counters'), true) : null,
        );
    }

    private function costs(JsonObject $o): CostsBlock
    {
        return new CostsBlock(
            profile: $o->string('profile'),
            gatesProfile: $o->bool('gates_profile'),
            rulesVersion: $o->string('rules_version'),
            feeGrid: $o->string('fee_grid'),
            seed: $o->int('seed'),
            costsFile: $o->string('costs_file'),
            costsFileHash: $o->string('costs_file_hash'),
            costMeasuresFile: $this->optionalString($o, 'cost_measures_file'),
            costMeasuresHash: $this->optionalString($o, 'cost_measures_hash'),
            stressCalibrated: $o->bool('stress_calibrated'),
            stressCalibrationRef: $this->optionalString($o, 'stress_calibration_ref'),
            legs: array_map($this->costLeg(...), $o->objectList('legs')),
        );
    }

    private function costLeg(JsonObject $o): CostLeg
    {
        return new CostLeg(
            symbol: $o->string('symbol'),
            class: $o->string('class'),
            classSource: $o->string('class_source'),
            spreadSource: $o->string('spread_source'),
            slippageSource: $o->string('slippage_source'),
            halfSpreadBps: $o->float('half_spread_bps'),
            marketSlippageBps: $o->float('market_slippage_bps'),
            measuredP95HalfSpreadBps: $this->optionalFloat($o, 'measured_p95_half_spread_bps'),
            stopSlippageBps: [
                'p50' => $o->float('stop_slippage_bps.p50'),
                'p95' => $o->float('stop_slippage_bps.p95'),
                'p99' => $o->float('stop_slippage_bps.p99'),
                'p100' => $o->float('stop_slippage_bps.p100'),
            ],
            stopSlippageMeanBps: $o->float('stop_slippage_mean_bps'),
            stressProbability: $o->float('stress_probability'),
            stressDrop: $o->float('stress_drop'),
            entry: $o->string('entry'),
            makerFillProbability: $this->optionalFloat($o, 'maker_fill_probability'),
            makerTtlMinutes: $this->optionalInt($o, 'maker_ttl_minutes'),
            adverseSelectionBps: $o->float('adverse_selection_bps'),
        );
    }

    /** Les clés du bloc sont celles de docs/specs/s10.md §12.2 : chacune est obligatoire, sa valeur peut être nulle. */
    private function costArithmetic(JsonObject $o): CostArithmetic
    {
        $breakdown = [];
        foreach ($o->scalarMap('cost_breakdown') as $name => $value) {
            if (!is_int($value) && !is_float($value)) {
                throw InvalidRunException::wrongType(
                    self::METRICS_FILE,
                    'cost_arithmetic.cost_breakdown.' . $name,
                    'un nombre',
                    get_debug_type($value),
                );
            }
            $breakdown[$name] = (float) $value;
        }

        return new CostArithmetic(
            decisionTf: $o->string('decision_tf'),
            protocolTf: $o->bool('protocol_tf'),
            roundTripCostMean: $o->nullableFloat('round_trip_cost_mean'),
            roundTripCostRateMean: $o->nullableFloat('round_trip_cost_rate_mean'),
            roundTripCostRateMedian: $o->nullableFloat('round_trip_cost_rate_median'),
            costBreakdown: $breakdown,
            costRMean: $o->nullableFloat('cost_r_mean'),
            costRMedian: $o->nullableFloat('cost_r_median'),
            tradesWithR: $o->int('trades_with_r'),
            costRLimit: $o->float('cost_r_limit'),
            costRPass: $this->nullableBool($o, 'cost_r_pass'),
            grossExpectancy: $o->nullableFloat('gross_expectancy'),
            costToGross: $o->nullableFloat('cost_to_gross'),
            costThirdPass: $o->bool('cost_third_pass'),
            grossMedianRate: $o->nullableFloat('gross_median_rate'),
            grossMedianVsOwnCostPass: $this->nullableBool($o, 'gross_median_vs_own_cost_pass'),
            tradesPerYear: $o->nullableFloat('trades_per_year'),
            capitalFractionMean: $o->nullableFloat('capital_fraction_mean'),
            hurdleAnnual: $o->nullableFloat('hurdle_annual'),
        );
    }

    /** `cost_counters` : la somme des jambes (avec `by_pair`) ou une ligne de `by_pair` (sans `symbol` au total). */
    private function costCounters(JsonObject $o, bool $withPairs): CostCounters
    {
        return new CostCounters(
            symbol: $withPairs ? null : $o->string('symbol'),
            stopFillsDrawn: $o->int('stop_fills_drawn'),
            stopStressFills: $o->int('stop_stress_fills'),
            makerEntriesConverted: $o->int('maker_entries_converted'),
            makerEntriesTakenAtArrival: $o->int('maker_entries_taken_at_arrival'),
            makerFillDraws: $o->int('maker_fill_draws'),
            makerFills: $o->int('maker_fills'),
            makerFillMissed: $o->int('maker_fill_missed'),
            makerEntriesFilledMaker: $o->int('maker_entries_filled_maker'),
            spreadCostTotal: $o->float('spread_cost_total'),
            adverseSelectionTotal: $o->float('adverse_selection_total'),
            makerEntryMakerShare: $o->nullableFloat('maker_entry_maker_share'),
            byPair: $withPairs
                ? array_map(fn (JsonObject $p): CostCounters => $this->costCounters($p, false), $o->objectList('by_pair'))
                : [],
        );
    }

    /** Booléen dont la clé est obligatoire et la valeur peut être nulle. */
    private function nullableBool(JsonObject $o, string $path): ?bool
    {
        return $o->isNull($path) ? null : $o->bool($path);
    }

    /**
     * @return list<PairFunnel>
     */
    private function funnelByPair(JsonObject $j): array
    {
        return array_map(
            static fn (JsonObject $o): PairFunnel => new PairFunnel(
                symbol: $o->string('symbol'),
                evaluations: $o->int('evaluations'),
                entrySignals: $o->int('entry_signals'),
                entryOrders: $o->int('entry_orders'),
                entryFills: $o->int('entry_fills'),
                trades: $o->int('trades'),
                rejected: $o->intMap('rejected'),
            ),
            $j->objectList('funnel_by_pair'),
        );
    }

    private function portfolioInfo(JsonObject $o): PortfolioInfo
    {
        return new PortfolioInfo(
            legs: $o->int('legs'),
            gridMinutes: $o->int('grid_minutes'),
            maxOpenPositions: $o->int('max_open_positions'),
            maxCostToInitial: $this->optionalFloat($o, 'max_cost_to_initial'),
            positionsCapHits: $o->int('positions_cap_hits'),
            quoteGapExits: $o->int('quote_gap_exits'),
            sizing: $o->string('sizing'),
            initialCash: $o->float('initial_cash'),
        );
    }

    private function priority(JsonObject $o): PriorityInfo
    {
        return new PriorityInfo(
            rule: $o->string('rule'),
            liquidityWindowDays: $o->int('liquidity_window_days'),
            rankSource: $o->string('rank_source'),
            rankExchange: $this->optionalString($o, 'rank_exchange'),
            rankDays: $o->int('rank_days'),
            partialDays: $o->int('partial_days'),
        );
    }

    private function regime(JsonObject $o): RegimeInfo
    {
        return new RegimeInfo(
            symbol: $o->string('symbol'),
            tf: $o->string('tf'),
            smaPeriod: $o->int('sma_period'),
            firstReadyMs: $this->optionalInt($o, 'first_ready_ms'),
            daysBull: $o->int('days_bull'),
            daysBear: $o->int('days_bear'),
        );
    }

    private function instruments(JsonObject $o): InstrumentsInfo
    {
        $changes = [];
        if ($o->has('upcoming_changes')) {
            foreach ($o->objectList('upcoming_changes') as $change) {
                $changes[] = new UpcomingChange(
                    instId: $change->string('inst_id'),
                    param: $change->string('param'),
                    newValue: $change->scalarString('new_value'),
                    effTime: $change->scalarString('eff_time'),
                );
            }
        }

        return new InstrumentsInfo(
            source: $o->string('source'),
            file: $this->optionalString($o, 'file'),
            instrumentsHash: $this->optionalString($o, 'instruments_hash'),
            fetchedAt: $this->optionalString($o, 'fetched_at'),
            notLive: $o->has('not_live') ? $o->stringList('not_live') : [],
            upcomingChanges: $changes,
        );
    }

    private function executability(JsonObject $o): Executability
    {
        $pairs = array_map(
            fn (JsonObject $p): ExecutabilityPair => new ExecutabilityPair(
                symbol: $p->string('symbol'),
                okxInstId: $this->optionalString($p, 'okx_inst_id'),
                entryOrders: $p->int('entry_orders'),
                notionalRef: $this->optionalNumber($p, 'notional_ref'),
                medianQuoteVolume1m: $this->optionalNumber($p, 'median_quote_volume_1m'),
                volumeCap: $this->optionalNumber($p, 'volume_cap'),
                minSz: $this->optionalNumber($p, 'min_sz'),
                medianClose: $this->optionalNumber($p, 'median_close'),
                minOrderNotional: $this->optionalNumber($p, 'min_order_notional'),
                state: $this->optionalString($p, 'state'),
                verdict: $p->string('verdict'),
            ),
            $o->has('pairs') ? $o->objectList('pairs') : [],
        );
        $rejected = array_map(
            static fn (JsonObject $r): ExecutabilityRejection => new ExecutabilityRejection($r->string('symbol'), $r->string('reason')),
            $o->has('rejected') ? $o->objectList('rejected') : [],
        );

        return new Executability(
            referenceCapital: $o->float('reference_capital'),
            rule: $o->string('rule'),
            venueMatchesExecution: $o->bool('venue_matches_execution'),
            pairs: $pairs,
            liveList: $o->has('live_list') ? $o->stringList('live_list') : [],
            rejected: $rejected,
        );
    }

    private function g1(JsonObject $o): G1Executable
    {
        $counts = [];
        $keys = ['fills', 'rejected_volume', 'rejected_min_notional', 'rejected_lot_step', 'rejected_cash', 'rejected_max_positions'];
        foreach ($keys as $key) {
            if ($o->has($key)) {
                $counts[$key] = $o->int($key);
            }
        }

        return new G1Executable(
            ratio: $this->optionalFloat($o, 'ratio'),
            threshold: $this->optionalFloat($o, 'threshold'),
            verdict: $o->string('verdict'),
            atReferenceCapital: $o->has('at_reference_capital') ? $o->bool('at_reference_capital') : false,
            counts: $counts,
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

    /** En s9, `entry_rejected_max_positions` est obligatoire (contrôlée par `tv4 verify`). */
    private function funnel(JsonObject $f, bool $portfolio): Funnel
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
            entryRejectedMaxPositions: $portfolio ? $f->int('entry_rejected_max_positions') : null,
        );
    }

    private function distribution(JsonObject $d, bool $portfolio): Distribution
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
            byPair: $portfolio && $d->has('by_pair') && !$d->isNull('by_pair') ? $this->byPair($d) : null,
        );
    }

    /**
     * @return list<PairStat>
     */
    private function byPair(JsonObject $d): array
    {
        return array_map(
            fn (JsonObject $o): PairStat => new PairStat(
                symbol: $o->string('symbol'),
                trades: $o->int('trades'),
                winRate: $this->optionalFloat($o, 'win_rate'),
                expectancyR: $this->optionalFloat($o, 'expectancy_r'),
                netPnl: $o->float('net_pnl'),
                exposure: $this->optionalFloat($o, 'exposure'),
            ),
            $d->objectList('by_pair'),
        );
    }
}
