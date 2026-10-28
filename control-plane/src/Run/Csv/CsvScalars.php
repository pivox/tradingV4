<?php

declare(strict_types=1);

namespace App\Run\Csv;

use App\Run\InvalidRunException;

/**
 * Conversion stricte des champs CSV écrits par `tv4` : entiers décimaux, flottants sans exposant
 * (`Display` de Rust), booléens `true` / `false`.
 */
final class CsvScalars
{
    private function __construct()
    {
    }

    public static function int(string $value, string $file, int $line, string $column): int
    {
        $parsed = filter_var($value, FILTER_VALIDATE_INT);
        if (false === $parsed) {
            throw InvalidRunException::invalidCsvLine($file, $line, sprintf('colonne %s : entier attendu, trouvé "%s"', $column, $value));
        }

        return $parsed;
    }

    public static function float(string $value, string $file, int $line, string $column): float
    {
        if (1 !== preg_match('/^-?\d+(\.\d+)?$/', $value)) {
            throw InvalidRunException::invalidCsvLine($file, $line, sprintf('colonne %s : nombre attendu, trouvé "%s"', $column, $value));
        }

        return (float) $value;
    }

    public static function bool(string $value, string $file, int $line, string $column): bool
    {
        return match ($value) {
            'true' => true,
            'false' => false,
            default => throw InvalidRunException::invalidCsvLine(
                $file,
                $line,
                sprintf('colonne %s : "true" ou "false" attendu, trouvé "%s"', $column, $value),
            ),
        };
    }
}
