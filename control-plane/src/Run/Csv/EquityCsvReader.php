<?php

declare(strict_types=1);

namespace App\Run\Csv;

use App\Run\Dto\EquitySeries;
use App\Run\InvalidRunException;

/**
 * Lecteur strict de `equity.csv` (`time_ms,equity,drawdown`). Trois listes de scalaires, pas un objet
 * par point : à `--equity-step 1m` le fichier approche le million de lignes.
 */
final class EquityCsvReader
{
    public const FILE = 'equity.csv';

    /** @var list<string> */
    public const COLUMNS = ['time_ms', 'equity', 'drawdown'];

    public function read(string $path): EquitySeries
    {
        $handle = @fopen($path, 'rb');
        if (false === $handle) {
            throw InvalidRunException::unreadableFile(self::FILE);
        }

        $times = [];
        $equity = [];
        $drawdown = [];
        try {
            $header = fgetcsv($handle, null, ',', '"', '');
            if (!is_array($header) || self::COLUMNS !== $header) {
                throw InvalidRunException::unexpectedHeader(
                    self::FILE,
                    implode(',', self::COLUMNS),
                    is_array($header) ? implode(',', array_map(static fn (?string $c): string => (string) $c, $header)) : '',
                );
            }

            $line = 1;
            while (false !== ($fields = fgetcsv($handle, null, ',', '"', ''))) {
                ++$line;
                if ([null] === $fields) {
                    continue;
                }
                if (3 !== count($fields)) {
                    throw InvalidRunException::invalidCsvLine(self::FILE, $line, sprintf('3 champs attendus, %d trouvés', count($fields)));
                }
                $times[] = CsvScalars::int((string) $fields[0], self::FILE, $line, 'time_ms');
                $equity[] = CsvScalars::float((string) $fields[1], self::FILE, $line, 'equity');
                $drawdown[] = CsvScalars::float((string) $fields[2], self::FILE, $line, 'drawdown');
            }
        } finally {
            fclose($handle);
        }

        return new EquitySeries($times, $equity, $drawdown);
    }
}
