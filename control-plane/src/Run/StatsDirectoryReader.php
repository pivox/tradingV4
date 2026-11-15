<?php

declare(strict_types=1);

namespace App\Run;

use App\Run\Dto\Manifest;
use App\Run\Dto\Metrics;
use App\Run\Dto\StatsReport;
use App\Run\Dto\TradeRow;
use App\Run\Json\JsonObject;
use App\Run\Stats\StatsValidator;

/**
 * Lit le sidecar optionnel et recoupe uniquement des déclarations déjà possédées.
 * @phpstan-import-type Concentration from StatsReport
 */
final class StatsDirectoryReader
{
    /** @param list<TradeRow> $trades */
    public function read(
        string $directory,
        Manifest $manifest,
        Metrics $metrics,
        array $trades,
        JsonObject $manifestJson,
        JsonObject $metricsJson,
    ): ?StatsReport {
        $file = $directory . '/stats.json';
        if (!file_exists($file) && !is_link($file)) {
            return null;
        }
        $bytes = @file_get_contents($file);
        if (false === $bytes) {
            throw InvalidRunException::unreadableFile('stats.json');
        }
        $stats = StatsValidator::decode($bytes);
        $data = $stats->data;
        $source = $data['source'];
        $p = $source['provenance'];
        foreach (
            [
            'run_id' => $manifest->runId,
            'config_hash' => $manifest->configHash,
            'dataset_hash' => $manifest->datasetHash,
            'result_hash' => $manifest->resultHash,
            'profile' => $manifest->profile,
            'line' => $metrics->research?->line,
            ] as $field => $value
        ) {
            StatsValidator::require($p[$field] === $value, 'source.provenance.' . $field, 'différent du run source');
        }
        StatsValidator::require(
            $p['config_hash'] === $metrics->configHash && $p['dataset_hash'] === $metrics->datasetHash,
            'source.provenance',
            'empreintes différentes des métriques',
        );
        $cash = null === $metrics->portfolio ? $manifest->config->initialCash : $metrics->portfolio->initialCash;
        $start = $metrics->isPortfolio() ? $metricsJson->int('data.window.start_ms') : $metricsJson->int('window.first_open_time_ms');
        $end = $metrics->isPortfolio() ? $metricsJson->int('data.window.end_ms') : $metricsJson->int('window.last_open_time_ms') + 60000;
        StatsValidator::require(
            StatsValidator::sameNumber($p['initial_cash'], $cash)
            && StatsValidator::sameNumber($p['initial_cash'], $manifestJson->float('config.initial_cash'))
            && StatsValidator::sameNumber($p['initial_cash'], $metricsJson->float('config.initial_cash')),
            'source.provenance.initial_cash',
            'capital différent du manifeste ou des métriques',
        );
        StatsValidator::require(
            $p['window']['start_ms'] === $manifest->window->startMs && $p['window']['end_ms'] === $manifest->window->endMs
            && $p['window']['start_ms'] === $start
            && $p['window']['end_ms'] === $end,
            'source.provenance.window',
            'fenêtre différente du manifeste ou des métriques',
        );
        foreach (['metrics.json' => 'metrics_json', 'trades.csv' => 'trades_csv', 'equity.csv' => 'equity_csv'] as $a => $b) {
            StatsValidator::require(
                ($manifest->fileHashes[$a] ?? null) === $source['files'][$b],
                'source.files.' . $b,
                'empreinte différente de outputs.files du manifeste',
            );
        }
        if ($manifestJson->has('stats_inputs')) {
            $packet = $manifestJson->object('stats_inputs');
            StatsValidator::require(
                'tv4-stats-inputs-v1' === $packet->string('schema') && 'stats-inputs.json' === $packet->string('file')
                && $packet->string('hash') === $source['files']['stats_inputs_json'],
                'source.files.stats_inputs_json',
                'référence du paquet différente du manifeste',
            );
        }
        $history = $manifestJson->has('governance') ? $manifestJson->value('governance') : null;
        StatsValidator::require(
            self::sameDeclaration(self::canonical($history), self::canonical($source['historical_governance'])),
            'source.historical_governance',
            'différent du manifeste',
        );
        $kept = array_values(array_filter(
            $trades,
            static fn (TradeRow $t): bool => null === $metrics->research || 'end_of_window' !== $t->exitType,
        ));
        StatsValidator::require(
            $data['bootstrap']['n_trades'] === count($kept)
            && $data['bootstrap']['n_r'] === count(array_filter($kept, static fn (TradeRow $t): bool => null !== $t->rMultiple))
            && count($data['regimes']['labels']) === count($trades),
            'bootstrap.n_trades',
            'effectifs différents du journal source',
        );
        StatsValidator::require(
            $data['monte_carlo']['sizing'] === (null === $metrics->portfolio ? 'compounding' : $metrics->portfolio->sizing),
            'monte_carlo.sizing',
            'sizing différent des métriques source',
        );
        $missingLabels = 0;
        $cells = ['bull-low' => 0, 'bull-high' => 0, 'bear-low' => 0, 'bear-high' => 0];
        foreach ($data['regimes']['labels'] as $label) {
            $id = $label['trade'];
            $trade = $trades[$id['row']] ?? null;
            if (
                null !== $trade && (null === $metrics->research || 'end_of_window' !== $trade->exitType)
                && (null === $label['trend'] || null === $label['volatility'])
            ) {
                ++$missingLabels;
            }
            if (
                null !== $trade && (null === $metrics->research || 'end_of_window' !== $trade->exitType)
                && null !== $label['trend'] && null !== $label['volatility']
            ) {
                ++$cells[$label['trend'] . '-' . $label['volatility']];
            }
            StatsValidator::require(
                null !== $trade && $trade->lotId === $id['lot_id'] && $trade->symbol === $id['symbol']
                && $trade->entryMs === $id['entry_ms'] && $trade->exitMs === $id['exit_ms'],
                'regimes.labels.trade',
                'identité différente du journal source',
            );
        }
        StatsValidator::require(
            $missingLabels === $data['regimes']['missing_labels'],
            'regimes.missing_labels',
            'effectif différent des étiquettes conservées',
        );
        foreach ($data['regimes']['cells'] as $cell) {
            StatsValidator::require(
                $cells[$cell['trend'] . '-' . $cell['volatility']] === $cell['n'],
                'regimes.cells.n',
                'effectifs différents des étiquettes du journal conservé',
            );
        }
        /** @var Concentration $concentration */
        $concentration = $data['concentration'];
        foreach ($concentration['without_top'] as $removal) {
            foreach ($removal['removed'] as $id) {
                $trade = $trades[$id['row']] ?? null;
                StatsValidator::require(
                    null !== $trade && $trade->lotId === $id['lot_id'] && $trade->symbol === $id['symbol']
                    && $trade->entryMs === $id['entry_ms'] && $trade->exitMs === $id['exit_ms']
                    && (null === $metrics->research || 'end_of_window' !== $trade->exitType),
                    'concentration.without_top.removed',
                    'identité absente ou purgée du journal source',
                );
            }
        }
        $effective = null === $manifest->governance ? $metrics->research?->block : $manifest->governance->effectiveBlock;
        if ('unclassified' !== $data['partition']) {
            StatsValidator::require(
                strtolower($effective ?? '') === $data['partition'] && $metrics->research?->block === $effective,
                'partition',
                'bloc différent de la provenance de recherche',
            );
        }
        return $stats;
    }
    /** Les doubles re-sérialisés par serde peuvent varier de quelques ULP (tolérance S8 : 20). */
    private static function sameDeclaration(mixed $left, mixed $right): bool
    {
        if (is_float($left) && is_float($right)) {
            return StatsValidator::sameNumber($left, $right);
        }
        if (is_array($left) && is_array($right)) {
            if (array_keys($left) !== array_keys($right)) {
                return false;
            }
            foreach ($left as $key => $value) {
                if (!self::sameDeclaration($value, $right[$key])) {
                    return false;
                }
            }
            return true;
        }
        return $left === $right;
    }

    private static function canonical(mixed $value): mixed
    {
        if (is_float($value) && is_finite($value) && $value >= PHP_INT_MIN && $value < PHP_INT_MAX && floor($value) === $value) {
            return (int) $value;
        }
        if (!is_array($value)) {
            return $value;
        }
        if (!array_is_list($value)) {
            ksort($value);
        }
        return array_map(self::canonical(...), $value);
    }
}
