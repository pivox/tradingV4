<?php

declare(strict_types=1);

namespace App\Run\Stats;

use App\Run\Dto\StatsReport;

/**
 * Cohérence structurelle des résultats, sans reproduire leurs calculs numériques.
 * @phpstan-import-type Metric from StatsReport
 * @phpstan-import-type Window from StatsReport
 * @phpstan-import-type Options from StatsReport
 * @phpstan-import-type Provenance from StatsReport
 * @phpstan-import-type Source from StatsReport
 * @phpstan-import-type Bootstrap from StatsReport
 * @phpstan-import-type MonteCarlo from StatsReport
 * @phpstan-import-type Daily from StatsReport
 * @phpstan-import-type TradeIdentity from StatsReport
 * @phpstan-import-type TradeRemoval from StatsReport
 * @phpstan-import-type Concentration from StatsReport
 * @phpstan-import-type RegimeLabel from StatsReport
 * @phpstan-import-type RegimeCell from StatsReport
 * @phpstan-import-type Regimes from StatsReport
 * @phpstan-import-type Bear from StatsReport
 * @phpstan-import-type Reference from StatsReport
 * @phpstan-import-type MonthCoverage from StatsReport
 * @phpstan-import-type DelaySample from StatsReport
 * @phpstan-import-type Delayed from StatsReport
 * @phpstan-import-type Registry from StatsReport
 * @phpstan-import-type Clustering from StatsReport
 * @phpstan-import-type Corpus from StatsReport
 * @phpstan-import-type StatsData from StatsReport
 */
final class StatsInvariants
{
    /** @param array<string, mixed> $v */
    public static function check(string $type, array $v, string $path): void
    {
        $ok = true;
        switch ($type) {
            case 'Metric':
            case 'DelaySection':
            case 'RegistrySection':
            case 'ClusteringSection':
                $ok = match ($v['availability']) {
                    'complete' => null !== $v['value'] && [] === $v['reasons'],
                    'unavailable' => null === $v['value'] && [] !== $v['reasons'],
                    default => null !== $v['value'] && [] !== $v['reasons'],
                };
                break;
            case 'Window':
            case 'DatedReturn':
                /** @var Window $v */
                $ok = $v['end_ms'] > $v['start_ms'];
                break;
            case 'Options':
                /** @var Options $v */
                $ok = $v['samples'] >= 10000 && $v['samples'] <= 4294967295
                    && $v['bootstrap_block'] >= 5 && $v['bootstrap_block'] <= 10 && $v['correlation_threshold'] < 1;
                break;
            case 'Provenance':
                /** @var Provenance $v */
                $ok = $v['result_hash'] === $v['files']['trades.csv'];
                break;
            case 'Source':
                /** @var Source $source */
                $source = $v;
                foreach (['metrics.json' => 'metrics_json', 'trades.csv' => 'trades_csv', 'equity.csv' => 'equity_csv'] as $a => $b) {
                    $ok = $ok && $source['provenance']['files'][$a] === $source['files'][$b];
                }
                $g = $source['historical_governance'];
                $ok = $ok && (null === $g || ($g['config_hash'] === $source['provenance']['config_hash']
                    && $g['dataset_hash'] === $source['provenance']['dataset_hash']));
                break;
            case 'Bootstrap':
                /** @var Bootstrap $v */
                $ok = $v['n_r'] <= $v['n_trades'] && self::nonnegative($v['standard_error_r'])
                    && $v['availability'] === self::availability([$v['mean_r'], $v['lb90_r'], $v['standard_error_r']]);
                if ('complete' === $v['availability']) {
                    $ok = $ok && $v['n_trades'] >= 2 && $v['n_r'] === $v['n_trades']
                        && null !== $v['mean_r']['value'] && null !== $v['lb90_r']['value']
                        && null !== $v['standard_error_r']['value'];
                }
                if (0 === $v['n_trades'] || $v['n_r'] < $v['n_trades']) {
                    $ok = $ok && 'unavailable' === $v['availability'] && null === $v['mean_r']['value']
                        && null === $v['lb90_r']['value'] && null === $v['standard_error_r']['value'];
                }
                $ok = $ok && (0 === $v['n_trades'] ? null === $v['r_coverage'] : null !== $v['r_coverage']);
                break;
            case 'TradeIdentity':
                /** @var TradeIdentity $v */
                $ok = $v['exit_ms'] >= $v['entry_ms'];
                break;
            case 'TradeRemoval':
                /** @var TradeRemoval $v */
                $ok = null === $v['pnl_without']['value'] ? [] === $v['removed'] : count($v['removed']) === $v['count'];
                $ok = $ok && count(array_unique(array_column($v['removed'], 'row'))) === count($v['removed']);
                break;
            case 'Concentration':
                /** @var Concentration $v */
                $ok = [1, 3, 5, 10] === array_column($v['without_top'], 'count');
                foreach ($v['without_top'] as $removal) {
                    $ok = $ok && ($v['n_trades'] > $removal['count'] ? null !== $removal['pnl_without']['value']
                        : null === $removal['pnl_without']['value']);
                }
                if ('complete' === $v['availability']) {
                    $ok = $ok && null !== $v['top3_share']['value'] && null !== $v['best_month_positive_share']['value']
                        && !in_array(null, array_column(array_column($v['without_top'], 'pnl_without'), 'value'), true);
                }
                $ok = $ok && (0 === $v['n_trades'] ? null === $v['best_month'] && null === $v['best_pair']
                    : null !== $v['best_month'] && null !== $v['best_pair']);
                break;
            case 'MonteCarlo':
                /** @var MonteCarlo $v */
                $ok = $v['n_r'] <= $v['n_trades'] && $v['n_removed'] === intdiv($v['n_trades'], 5)
                    && $v['n_blocks'] === count($v['block_sizes']) && $v['n_blocks'] === count($v['block_durations_ms'])
                    && 20 === $v['requested_block_trades'] && 31557600000 === $v['horizon_one_year_ms']
                    && self::nonnegative($v['dd_initial_p95_2y']) && self::bounded($v['dd_peak_p95_2y'], 0, 1)
                    && self::bounded($v['probability_dd20_1y'], 0, 1) && self::bounded($v['return_p5_2y'], -1);
                if ($v['n_blocks'] > 0) {
                    $ok = $ok && array_sum($v['block_sizes']) === $v['n_trades']
                        && array_sum($v['block_durations_ms']) === $v['source_duration_ms']
                        && !in_array(0, $v['block_sizes'], true) && !in_array(0, $v['block_durations_ms'], true);
                }
                $ok = $ok && $v['availability'] === self::availability([
                    $v['dd_initial_p95_2y'], $v['dd_peak_p95_2y'], $v['return_p5_2y'],
                    $v['removal_expectancy_median_r'], $v['probability_dd20_1y'],
                ]);
                if ('complete' === $v['availability']) {
                    $ok = $ok && $v['n_blocks'] >= 2 && null !== $v['dd_initial_p95_2y']['value']
                        && null !== $v['dd_peak_p95_2y']['value'] && null !== $v['return_p5_2y']['value'];
                }
                break;
            case 'Daily':
                /** @var Daily $v */
                $ok = count($v['intervals']) + count($v['missing_intervals']) === $v['expected_intervals'];
                $seen = [];
                foreach ([$v['intervals'], $v['missing_intervals']] as $intervals) {
                    $last = null;
                    foreach ($intervals as $w) {
                        $ok = $ok && 86400000 === $w['end_ms'] - $w['start_ms'] && 0 === $w['start_ms'] % 86400000
                            && (null === $last || $w['start_ms'] >= $last) && !isset($seen[$w['start_ms']]);
                        $seen[$w['start_ms']] = true;
                        $last = $w['end_ms'];
                    }
                }
                $ok = $ok && (0 === $v['expected_intervals'] ? null === $v['coverage']
                    : null !== $v['coverage']
                    && StatsValidator::sameNumber($v['coverage'] * $v['expected_intervals'], count($v['intervals'])));
                break;
            case 'RegimeLabel':
                /** @var RegimeLabel $v */
                $ok = (null === $v['close_ms'] || $v['close_ms'] <= $v['trade']['entry_ms'])
                    && self::nonnegative($v['volatility30']) && self::nonnegative($v['expanding_median']);
                break;
            case 'RegimeCell':
                /** @var RegimeCell $v */
                $ok = $v['n_r'] <= $v['n'] && (0 !== $v['n'] && $v['n'] === $v['n_r']
                    ? null !== $v['mu_r']['value'] && null !== $v['below_expectancy_threshold']
                    : null === $v['mu_r']['value'] && null === $v['below_expectancy_threshold']);
                break;
            case 'Regimes':
                /** @var Regimes $v */
                $pairs = array_map(static fn (array $cell): string => $cell['trend'] . '-' . $cell['volatility'], $v['cells']);
                $ok = ['bull-low', 'bull-high', 'bear-low', 'bear-high'] === $pairs
                    && count(array_unique(array_column(array_column($v['labels'], 'trade'), 'row'))) === count($v['labels'])
                    && $v['missing_labels'] <= count($v['labels']) && $v['missing_r'] <= count($v['labels']);
                break;
            case 'Bear':
                /** @var Bear $v */
                $ok = $v['n_intervals'] === count($v['intervals'])
                    && (null === $v['qualification'] ? [] !== $v['qualification_reasons'] : [] === $v['qualification_reasons'])
                    && self::bounded($v['annualized_return'], -1);
                break;
            case 'Reference':
                /** @var Reference $v */
                $ok = self::bounded($v['total_return'], -1);
                $last = null;
                foreach ($v['trajectory'] as $point) {
                    $ok = $ok && (null === $last || $point['at_ms'] > $last);
                    $last = $point['at_ms'];
                }
                break;
            case 'MonthCoverage':
                /** @var MonthCoverage $v */
                $ok = $v['observed_intervals'] <= $v['expected_intervals'] && [] !== $v['reasons'];
                break;
            case 'DelaySample':
                /** @var DelaySample $v */
                $ok = $v['n_r'] <= $v['n'] && (0 === $v['n'] || $v['n_r'] !== $v['n']
                    ? null === $v['mean_r_net']['value'] : null !== $v['mean_r_net']['value']);
                break;
            case 'Delayed':
                /** @var Delayed $v */
                $ok = in_array($v['delay_minutes'], [1, 3, 5, 15, 30, 60, 120, 240, 360, 720, 1440], true)
                    && ($v['eligible_dev_g2'] ? [] === $v['eligibility_reasons'] : [] !== $v['eligibility_reasons']);
                foreach (['dataset_hash', 'initial_cash', 'window', 'profile', 'line'] as $field) {
                    $ok = $ok && $v['original']['source'][$field] === $v['delayed']['source'][$field];
                }
                break;
            case 'Registry':
                /** @var Registry $v */
                $ok = count($v['registered_hashes']) === $v['declared_n']
                    && count(array_unique($v['registered_hashes'])) === $v['declared_n'];
                break;
            case 'Clustering':
                /** @var Clustering $v */
                $flat = [];
                foreach ($v['clusters'] as $cluster) {
                    $ok = $ok && [] !== $cluster;
                    array_push($flat, ...$cluster);
                }
                $ok = $ok && $v['panel_n_eff'] === count($v['clusters']) && count($flat) === $v['observed_configs']
                    && count(array_unique($flat)) === count($flat) && $v['correlation_threshold'] < 1;
                $ok = $ok && self::coverage($v['coverage'], $v['observed_configs'], $v['declared_verified_n']);
                $full = null !== $v['declared_verified_n'] && $v['declared_verified_n'] > 0
                    && $v['observed_configs'] === $v['declared_verified_n'];
                $ok = $ok && ($full ? $v['global_n_eff']['value'] === $v['panel_n_eff']
                    || $v['global_n_eff']['value'] === (float) $v['panel_n_eff'] : null === $v['global_n_eff']['value']);
                $ok = $ok && (null === $v['declared_verified_n'] || $v['declared_verified_n'] >= $v['observed_configs']);
                break;
            case 'Corpus':
                /** @var Corpus $v */
                $configs = array_map(static fn (array $m): string => $m['source']['provenance']['config_hash'], $v['members']);
                $ok = count(array_unique($configs)) === $v['observed_configs'] && $v['complete_daily_configs'] <= $v['observed_configs']
                    && (null === $v['declared_verified_n'] || $v['declared_verified_n'] >= $v['observed_configs']);
                $ok = $ok && self::coverage($v['coverage'], $v['observed_configs'], $v['declared_verified_n']);
                $complete = [];
                foreach ($v['members'] as $member) {
                    if ($member['daily']['expected_intervals'] > 0 && [] === $member['daily']['missing_intervals']) {
                        $complete[$member['source']['provenance']['config_hash']] = true;
                    }
                }
                $ok = $ok && count($complete) === $v['complete_daily_configs'];
                $clustering = $v['clustering']['value'];
                $ok = $ok && (null === $clustering || ($clustering['observed_configs'] === $v['complete_daily_configs']
                    && $clustering['declared_verified_n'] === $v['declared_verified_n']));
                break;
            case 'StatsData':
                /** @var StatsData $root */
                $root = $v;
                /** @var Corpus $corpus */
                $corpus = $root['corpus'];
                $ok = $root['bootstrap']['n_trades'] === $root['concentration']['n_trades']
                    && $root['bootstrap']['n_trades'] === $root['monte_carlo']['n_trades']
                    && $root['bootstrap']['n_r'] === $root['monte_carlo']['n_r']
                    && $root['regimes']['missing_r'] === $root['bootstrap']['n_trades'] - $root['bootstrap']['n_r'];
                foreach (
                    [$root['benchmarks']['btc'], $root['benchmarks']['basket_50_50'], $root['benchmarks']['equal_weight_pairs'],
                    ...array_column($root['benchmarks']['pairs'], 'benchmark')] as $reference
                ) {
                    $ok = $ok && ('dev' === $root['partition'] || null === $reference['monthly']['lb95_excess']['value']);
                }
                $delay = $root['delayed_entries']['value'];
                $ok = $ok && (null === $delay ? null === $root['delayed_source'] : null !== $root['delayed_source']
                    && $delay['original']['source'] === $root['source']['provenance']
                    && $delay['delayed']['source'] === $root['delayed_source']['provenance']);
                /** @var Registry|null $registry */
                $registry = $root['registry']['value'];
                if (null !== $registry) {
                    $allRegistered = $registry['declared_n'] > 0;
                    foreach ($corpus['members'] as $member) {
                        $registered = in_array(
                            $member['source']['provenance']['config_hash'],
                            $registry['registered_hashes'],
                            true,
                        );
                        $ok = $ok && $member['registry_member'] === $registered;
                        $allRegistered = $allRegistered && $registered;
                    }
                    $ok = $ok && $corpus['declared_verified_n'] === ($allRegistered ? $registry['declared_n'] : null);
                } else {
                    $ok = $ok && null === $root['corpus']['declared_verified_n'];
                }
                $ok = $ok && (null === $corpus['declared_verified_n']) === (null === $root['current_noise']);
                $global = $corpus['clustering']['value']['global_n_eff']['value'] ?? null;
                $noiseEff = $root['current_noise']['n_eff'] ?? null;
                $ok = $ok && (null === $global ? null === $noiseEff : null !== $noiseEff && $noiseEff == $global);
                $ok = $ok && (null === $root['current_noise'] || (null !== $registry
                    && $root['current_noise']['n'] === $registry['declared_n']
                    && 5.0 === (float) $root['current_noise']['selection_years']));
                break;
        }
        StatsValidator::require($ok, $path, 'statuts, bornes ou cardinalités incohérents (' . $type . ')');
    }

    /** @param non-empty-list<Metric> $metrics */
    private static function availability(array $metrics): string
    {
        $present = count(array_filter($metrics, static fn (array $metric): bool => null !== $metric['value']));
        return 0 === $present ? 'unavailable' : ($present === count($metrics) ? 'complete' : 'partial');
    }

    private static function coverage(?float $coverage, int $observed, ?int $declared): bool
    {
        return null === $declared || 0 === $declared ? null === $coverage
            : null !== $coverage && StatsValidator::sameNumber($coverage * $declared, $observed);
    }

    /** @param Metric $metric */
    private static function nonnegative(array $metric): bool
    {
        return self::bounded($metric, 0);
    }

    /** @param Metric $metric */
    private static function bounded(array $metric, float $min, float $max = INF): bool
    {
        return null === $metric['value'] || ($metric['value'] >= $min && $metric['value'] <= $max);
    }
}
