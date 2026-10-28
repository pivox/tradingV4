<?php

declare(strict_types=1);

namespace App\Run\Csv;

use App\Run\Dto\TradeRow;
use App\Run\InvalidRunException;

/**
 * Lecteur strict de `trades.csv` (36 colonnes, docs/03-architecture.md §7.2 et schéma S8).
 */
final class TradesCsvReader
{
    public const FILE = 'trades.csv';

    /** @var list<string> */
    public const COLUMNS = [
        'lot_id', 'entry_signal_ms', 'entry_ms', 'entry_liquidity', 'entry_ref_price', 'entry_price',
        'entry_fee', 'entry_slippage', 'exit_signal_ms', 'exit_ms', 'exit_reason', 'exit_liquidity',
        'exit_ref_price', 'exit_price', 'exit_fee', 'exit_slippage', 'qty', 'pnl_net', 'mfe', 'mae',
        'stop_at_exit', 'capped_by_cash', 'forced_close', 'entry_tag', 'exit_tag', 'symbol',
        'entry_order_ms', 'exit_order_ms', 'exit_type', 'holding_ms', 'position_time_share',
        'stop_at_entry', 'risk_quote', 'r_multiple', 'regime_at_entry', 'spread_cost',
    ];

    /** @var list<string> */
    public const LIQUIDITIES = ['maker', 'taker'];

    /** @var list<string> */
    public const EXIT_REASONS = ['signal', 'limit_sell', 'stop', 'take_profit', 'forced_close'];

    /** Vocabulaire de `exit_type` écrit au jalon 1. @var list<string> */
    public const EXIT_TYPES = [
        'strategy_exit', 'time_stop', 'limit_sell', 'stop_loss', 'trailing_stop', 'take_profit', 'end_of_window',
    ];

    /** Colonnes dont la valeur peut être vide. @var list<string> */
    private const NULLABLE = ['stop_at_exit', 'stop_at_entry', 'risk_quote', 'r_multiple', 'regime_at_entry', 'spread_cost'];

    /**
     * @return list<TradeRow>
     */
    public function read(string $path): array
    {
        $handle = @fopen($path, 'rb');
        if (false === $handle) {
            throw InvalidRunException::unreadableFile(self::FILE);
        }

        try {
            $header = fgetcsv($handle, null, ',', '"', '');
            if (!is_array($header) || self::COLUMNS !== $header) {
                throw InvalidRunException::unexpectedHeader(
                    self::FILE,
                    implode(',', self::COLUMNS),
                    is_array($header) ? implode(',', array_map(static fn (?string $c): string => (string) $c, $header)) : '',
                );
            }

            $trades = [];
            $line = 1;
            while (false !== ($fields = fgetcsv($handle, null, ',', '"', ''))) {
                ++$line;
                if ([null] === $fields) {
                    continue;
                }
                if (count($fields) !== count(self::COLUMNS)) {
                    throw InvalidRunException::invalidCsvLine(
                        self::FILE,
                        $line,
                        sprintf('%d champs attendus, %d trouvés', count(self::COLUMNS), count($fields)),
                    );
                }
                $trades[] = $this->row(array_map(static fn (?string $f): string => (string) $f, $fields), $line);
            }
        } finally {
            fclose($handle);
        }

        return $trades;
    }

    /**
     * @param list<string> $f
     */
    private function row(array $f, int $line): TradeRow
    {
        $by = array_combine(self::COLUMNS, $f);
        foreach ($by as $column => $value) {
            if ('' === $value && !in_array($column, self::NULLABLE, true) && !in_array($column, ['entry_tag', 'exit_tag'], true)) {
                throw InvalidRunException::invalidCsvLine(self::FILE, $line, sprintf('colonne %s : valeur vide interdite', $column));
            }
        }

        $int = static fn (string $c): int => CsvScalars::int($by[$c], self::FILE, $line, $c);
        $float = static fn (string $c): float => CsvScalars::float($by[$c], self::FILE, $line, $c);
        $bool = static fn (string $c): bool => CsvScalars::bool($by[$c], self::FILE, $line, $c);
        $optFloat = static fn (string $c): ?float => '' === $by[$c] ? null : CsvScalars::float($by[$c], self::FILE, $line, $c);
        return new TradeRow(
            lotId: $int('lot_id'),
            entrySignalMs: $int('entry_signal_ms'),
            entryMs: $int('entry_ms'),
            entryLiquidity: $this->oneOf($by['entry_liquidity'], 'entry_liquidity', self::LIQUIDITIES, $line),
            entryRefPrice: $float('entry_ref_price'),
            entryPrice: $float('entry_price'),
            entryFee: $float('entry_fee'),
            entrySlippage: $float('entry_slippage'),
            exitSignalMs: $int('exit_signal_ms'),
            exitMs: $int('exit_ms'),
            exitReason: $this->oneOf($by['exit_reason'], 'exit_reason', self::EXIT_REASONS, $line),
            exitLiquidity: $this->oneOf($by['exit_liquidity'], 'exit_liquidity', self::LIQUIDITIES, $line),
            exitRefPrice: $float('exit_ref_price'),
            exitPrice: $float('exit_price'),
            exitFee: $float('exit_fee'),
            exitSlippage: $float('exit_slippage'),
            qty: $float('qty'),
            pnlNet: $float('pnl_net'),
            mfe: $float('mfe'),
            mae: $float('mae'),
            stopAtExit: $optFloat('stop_at_exit'),
            cappedByCash: $bool('capped_by_cash'),
            forcedClose: $bool('forced_close'),
            entryTag: $by['entry_tag'],
            exitTag: $by['exit_tag'],
            symbol: $by['symbol'],
            entryOrderMs: $int('entry_order_ms'),
            exitOrderMs: $int('exit_order_ms'),
            exitType: $this->oneOf($by['exit_type'], 'exit_type', self::EXIT_TYPES, $line),
            holdingMs: $int('holding_ms'),
            positionTimeShare: $float('position_time_share'),
            stopAtEntry: $optFloat('stop_at_entry'),
            riskQuote: $optFloat('risk_quote'),
            rMultiple: $optFloat('r_multiple'),
            regimeAtEntry: '' === $by['regime_at_entry'] ? null : $by['regime_at_entry'],
            spreadCost: $optFloat('spread_cost'),
        );
    }

    /**
     * @param list<string> $allowed
     */
    private function oneOf(string $value, string $column, array $allowed, int $line): string
    {
        if (!in_array($value, $allowed, true)) {
            throw InvalidRunException::invalidCsvLine(
                self::FILE,
                $line,
                sprintf('colonne %s : valeur inconnue "%s" (attendu : %s)', $column, $value, implode(', ', $allowed)),
            );
        }

        return $value;
    }
}
