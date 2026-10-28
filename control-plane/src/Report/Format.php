<?php

declare(strict_types=1);

namespace App\Report;

use DateTimeImmutable;
use DateTimeZone;

/**
 * Mise en forme française des valeurs du rapport : montants à 2 décimales, ratios en pourcentage à
 * 2 décimales, dates `AAAA-MM-JJ HH:MM UTC` depuis des millisecondes.
 */
final class Format
{
    private const THOUSANDS = "\u{202F}";
    private const DECIMAL = ',';
    private const MINUS = "\u{2212}";

    public function number(?float $value, int $decimals = 2): string
    {
        if (null === $value) {
            return '—';
        }
        $text = number_format(abs($value), $decimals, self::DECIMAL, self::THOUSANDS);
        $negative = $value < 0 && 1 === preg_match('/[1-9]/', $text);

        return ($negative ? self::MINUS : '') . $text;
    }

    public function money(?float $value): string
    {
        return $this->number($value, 2);
    }

    /** Prix : 2 décimales au moins, jusqu'à 8 pour les petites quantités. */
    public function quantity(?float $value): string
    {
        return $this->number($value, 8);
    }

    /** Fraction → pourcentage : 0,0123 donne « 1,23 % ». */
    public function percent(?float $fraction): string
    {
        if (null === $fraction) {
            return '—';
        }

        return $this->number($fraction * 100, 2) . "\u{00A0}%";
    }

    public function integer(?int $value): string
    {
        if (null === $value) {
            return '—';
        }

        return $this->number((float) $value, 0);
    }

    public function dateTime(?int $ms): string
    {
        if (null === $ms) {
            return '—';
        }

        return $this->utc($ms)->format('Y-m-d H:i') . ' UTC';
    }

    public function date(int $ms): string
    {
        return $this->utc($ms)->format('Y-m-d');
    }

    /** Étiquette d'axe : `HH:MM` sous 2 jours, `MM-JJ` sous un an, `AAAA-MM` au-delà. */
    public function axisDate(int $ms, int $spanMs): string
    {
        $format = match (true) {
            $spanMs < 2 * 86_400_000 => 'H:i',
            $spanMs < 365 * 86_400_000 => 'm-d',
            default => 'Y-m',
        };

        return $this->utc($ms)->format($format);
    }

    public function year(int $ms): int
    {
        return (int) $this->utc($ms)->format('Y');
    }

    /** Durée en millisecondes → « 1 j 3 h », « 5 h 30 min », « 12 min ». */
    public function duration(?float $ms): string
    {
        if (null === $ms) {
            return '—';
        }
        if ($ms < 60_000) {
            return (int) round($ms / 1000) . ' s';
        }
        $minutes = (int) round($ms / 60_000);
        $days = intdiv($minutes, 1440);
        $hours = intdiv($minutes % 1440, 60);
        $mins = $minutes % 60;
        if ($days > 0) {
            return sprintf('%d j %d h', $days, $hours);
        }
        if ($hours > 0) {
            return 0 === $mins ? sprintf('%d h', $hours) : sprintf('%d h %d min', $hours, $mins);
        }

        return sprintf('%d min', $mins);
    }

    /** Valeur scalaire de configuration, telle quelle (booléens en toutes lettres). */
    public function scalar(bool|int|float|string|null $value): string
    {
        return match (true) {
            null === $value => 'null',
            is_bool($value) => $value ? 'true' : 'false',
            is_int($value) => (string) $value,
            is_float($value) => rtrim(rtrim(sprintf('%.10F', $value), '0'), '.') ?: '0',
            default => $value,
        };
    }

    private function utc(int $ms): DateTimeImmutable
    {
        return (new DateTimeImmutable('@' . intdiv($ms, 1000)))->setTimezone(new DateTimeZone('UTC'));
    }
}
