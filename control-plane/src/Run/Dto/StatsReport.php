<?php

declare(strict_types=1);

namespace App\Run\Dto;

/**
 * Statistiques déjà validées : valeurs Rust, sans recalcul dans le control plane.
 *
 * @phpstan-type Window array{
 *     'start_ms': int,
 *     'end_ms': int,
 * }
 * @phpstan-type Metric array{
 *     'availability': 'complete'|'partial'|'unavailable',
 *     'value': float|int|null,
 *     'reasons': list<string>,
 * }
 * @phpstan-type Options array{
 *     'seed': string,
 *     'samples': non-negative-int,
 *     'bootstrap_block': non-negative-int,
 *     'correlation_threshold': float|int,
 * }
 * @phpstan-type SimulationHashes array{
 *     'metrics.json': string,
 *     'trades.csv': string,
 *     'equity.csv': string,
 * }
 * @phpstan-type Provenance array{
 *     'run_id': string,
 *     'config_hash': string,
 *     'dataset_hash': string,
 *     'result_hash': string,
 *     'initial_cash': float|int,
 *     'window': Window,
 *     'profile': 'baseline'|'adverse'|'maker'|null,
 *     'line': 'full'|'okx_usdc'|null,
 *     'files': SimulationHashes,
 * }
 * @phpstan-type SourceFiles array{
 *     'manifest_json': string,
 *     'metrics_json': string,
 *     'trades_csv': string,
 *     'equity_csv': string,
 *     'stats_inputs_json': string|null,
 * }
 * @phpstan-type LayoutFile array{
 *     'file': string,
 *     'hash': string,
 * }
 * @phpstan-type Noise array{
 *     'n': non-negative-int,
 *     'n_eff': non-negative-int|null,
 *     'selection_years': float|int,
 *     'expected': float|int|null,
 *     'approx_q90': float|int|null,
 *     'q95': float|int|null,
 * }
 * @phpstan-type Hypothesis array{
 *     'identifiant': string,
 *     'sha256': string,
 * }
 * @phpstan-type Governance array{
 *     'schema': 'tv4-governance-v1',
 *     'config_hash': string,
 *     'dataset_hash': string,
 *     'trial_id': non-negative-int,
 *     'read_id': non-negative-int,
 *     'hypothesis': Hypothesis|null,
 *     'frozen_tag': string|null,
 *     'frozen_at': string|null,
 *     'source_block': 'DEV'|'VALIDATION'|'HOLDOUT'|'FORWARD'|null,
 *     'effective_block': 'DEV'|'VALIDATION'|'HOLDOUT'|'FORWARD'|null,
 *     'generation': non-negative-int,
 *     'claim_holdout': non-negative-int|null,
 *     'noise': Noise,
 * }
 * @phpstan-type Source array{
 *     'provenance': Provenance,
 *     'files': SourceFiles,
 *     'layout_files': list<LayoutFile>,
 *     'historical_governance': Governance|null,
 * }
 * @phpstan-type Selection array{
 *     'btc': string|null,
 *     'eth': string|null,
 *     'quote': string|null,
 *     'reasons': list<string>,
 * }
 * @phpstan-type Bootstrap array{
 *     'availability': 'complete'|'partial'|'unavailable',
 *     'n_trades': non-negative-int,
 *     'n_r': non-negative-int,
 *     'r_coverage': float|int|null,
 *     'mean_r': Metric,
 *     'lb90_r': Metric,
 *     'standard_error_r': Metric,
 * }
 * @phpstan-type TradeIdentity array{
 *     'symbol': string,
 *     'lot_id': non-negative-int,
 *     'entry_ms': int,
 *     'exit_ms': int,
 *     'row': non-negative-int,
 * }
 * @phpstan-type GroupRemoval array{
 *     'id': string,
 *     'pnl': float|int,
 *     'pnl_without': float|int,
 * }
 * @phpstan-type TradeRemoval array{
 *     'count': non-negative-int,
 *     'removed': list<TradeIdentity>,
 *     'pnl_without': Metric,
 * }
 * @phpstan-type Concentration array{
 *     'availability': 'complete'|'partial'|'unavailable',
 *     'n_trades': non-negative-int,
 *     'total_pnl': float|int,
 *     'best_month': GroupRemoval|null,
 *     'best_pair': GroupRemoval|null,
 *     'without_top': list<TradeRemoval>,
 *     'top3_share': Metric,
 *     'best_month_positive_share': Metric,
 * }
 * @phpstan-type MonteCarlo array{
 *     'availability': 'complete'|'partial'|'unavailable',
 *     'n_trades': non-negative-int,
 *     'n_r': non-negative-int,
 *     'n_removed': non-negative-int,
 *     'requested_block_trades': non-negative-int,
 *     'n_blocks': non-negative-int,
 *     'block_sizes': list<non-negative-int>,
 *     'block_durations_ms': list<non-negative-int>,
 *     'source_duration_ms': non-negative-int,
 *     'cadence_trades_per_year': float|int|null,
 *     'sizing': 'research'|'compounding',
 *     'horizon_one_year_ms': non-negative-int,
 *     'drawdown_grid': string,
 *     'dd_initial_p95_2y': Metric,
 *     'dd_peak_p95_2y': Metric,
 *     'return_p5_2y': Metric,
 *     'removal_expectancy_median_r': Metric,
 *     'probability_dd20_1y': Metric,
 * }
 * @phpstan-type DatedReturn array{
 *     'start_ms': int,
 *     'end_ms': int,
 *     'value': float|int,
 * }
 * @phpstan-type Daily array{
 *     'intervals': list<DatedReturn>,
 *     'expected_intervals': non-negative-int,
 *     'missing_intervals': list<Window>,
 *     'coverage': float|int|null,
 * }
 * @phpstan-type RegimeLabel array{
 *     'trade': TradeIdentity,
 *     'close_ms': int|null,
 *     'trend': 'bull'|'bear'|null,
 *     'volatility': 'low'|'high'|null,
 *     'volatility30': Metric,
 *     'expanding_median': Metric,
 *     'reasons': list<string>,
 * }
 * @phpstan-type RegimeCell array{
 *     'trend': 'bull'|'bear',
 *     'volatility': 'low'|'high',
 *     'n': non-negative-int,
 *     'n_r': non-negative-int,
 *     'mu_r': Metric,
 *     'below_expectancy_threshold': bool|null,
 * }
 * @phpstan-type Bear array{
 *     'n_entries': non-negative-int,
 *     'mu_r': Metric,
 *     'n_intervals': non-negative-int,
 *     'intervals': list<Window>,
 *     'duration_years': float|int,
 *     'missing_regime_intervals': list<Window>,
 *     'missing_bear_intervals': list<Window>,
 *     'annualized_return': Metric,
 *     'qualification': 'flat'|'positive'|'negative'|null,
 *     'qualification_reasons': list<string>,
 * }
 * @phpstan-type Regimes array{
 *     'labels': list<RegimeLabel>,
 *     'cells': list<RegimeCell>,
 *     'missing_labels': non-negative-int,
 *     'missing_r': non-negative-int,
 *     'volatility_origin_ms': int|null,
 *     'daily': Daily,
 *     'bear': Bear,
 * }
 * @phpstan-type Price array{
 *     'at_ms': int,
 *     'close': float|int,
 * }
 * @phpstan-type MonthCoverage array{
 *     'window': Window,
 *     'expected_intervals': non-negative-int,
 *     'observed_intervals': non-negative-int,
 *     'coverage': float|int,
 *     'reasons': list<string>,
 * }
 * @phpstan-type Monthly array{
 *     'method': string,
 *     'used_months': list<Window>,
 *     'excluded_months': list<MonthCoverage>,
 *     'point_excess': Metric,
 *     'lb95_excess': Metric,
 * }
 * @phpstan-type Reference array{
 *     'id': string,
 *     'trajectory': list<Price>,
 *     'total_return': Metric,
 *     'daily': Daily,
 *     'excess_sharpe': Metric,
 *     'monthly': Monthly,
 *     'calmar': Metric,
 * }
 * @phpstan-type PairBenchmark array{
 *     'symbol': string,
 *     'exposure_fraction': float|int,
 *     'benchmark': Reference,
 * }
 * @phpstan-type Benchmarks array{
 *     'cash_convention': string,
 *     'pair_aggregate_convention': string,
 *     'cash_reference_quote': Metric,
 *     'cash_reference_return': Metric,
 *     'strategy_total_return': Metric,
 *     'strategy_daily': Daily,
 *     'strategy_calmar': Metric,
 *     'btc': Reference,
 *     'basket_50_50': Reference,
 *     'pairs': list<PairBenchmark>,
 *     'equal_weight_pairs': Reference,
 * }
 * @phpstan-type DelaySample array{
 *     'source': Provenance,
 *     'n': non-negative-int,
 *     'n_r': non-negative-int,
 *     'mean_r_net': Metric,
 * }
 * @phpstan-type Delayed array{
 *     'original': DelaySample,
 *     'delayed': DelaySample,
 *     'delay_minutes': non-negative-int,
 *     'delayed_to_original_ratio': Metric,
 *     'eligible_dev_g2': bool,
 *     'eligibility_reasons': list<string>,
 * }
 * @phpstan-type Registry array{
 *     'registry_hash': string,
 *     'last_event_id': non-negative-int|null,
 *     'declared_n': non-negative-int,
 *     'registered_hashes': list<string>,
 * }
 * @phpstan-type Member array{
 *     'source': Source,
 *     'registry_member': bool|null,
 *     'daily': Daily,
 * }
 * @phpstan-type Clustering array{
 *     'method': string,
 *     'correlation_threshold': float|int,
 *     'observed_configs': non-negative-int,
 *     'declared_verified_n': non-negative-int|null,
 *     'coverage': float|int|null,
 *     'clusters': list<list<string>>,
 *     'panel_n_eff': non-negative-int,
 *     'global_n_eff': Metric,
 * }
 * @phpstan-type DelaySection array{
 *     'availability': 'complete'|'partial'|'unavailable',
 *     'value': Delayed|null,
 *     'reasons': list<string>,
 * }
 * @phpstan-type RegistrySection array{
 *     'availability': 'complete'|'partial'|'unavailable',
 *     'value': Registry|null,
 *     'reasons': list<string>,
 * }
 * @phpstan-type ClusteringSection array{
 *     'availability': 'complete'|'partial'|'unavailable',
 *     'value': Clustering|null,
 *     'reasons': list<string>,
 * }
 * @phpstan-type Corpus array{
 *     'members': list<Member>,
 *     'observed_configs': non-negative-int,
 *     'complete_daily_configs': non-negative-int,
 *     'declared_verified_n': non-negative-int|null,
 *     'coverage': float|int|null,
 *     'reasons': list<string>,
 *     'clustering': ClusteringSection,
 * }
 * @phpstan-type StatsData array{
 *     'schema': 'tv4-stats-v1',
 *     'options': Options,
 *     'source': Source,
 *     'layout_files': list<LayoutFile>,
 *     'partition': 'dev'|'validation'|'holdout'|'forward'|'unclassified',
 *     'selection': Selection,
 *     'bootstrap': Bootstrap,
 *     'concentration': Concentration,
 *     'monte_carlo': MonteCarlo,
 *     'regimes': Regimes,
 *     'benchmarks': Benchmarks,
 *     'delayed_entries': DelaySection,
 *     'delayed_source': Source|null,
 *     'registry': RegistrySection,
 *     'current_noise': Noise|null,
 *     'corpus': Corpus,
 *     'methods': list<string>,
 * }
 */
final readonly class StatsReport
{
    /** @param StatsData $data */
    public function __construct(public array $data)
    {
    }
}
