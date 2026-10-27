<?php

declare(strict_types=1);

namespace App\Report\Chart;

/**
 * Décimation min-max : au-delà de `threshold` points, la série est découpée en `buckets` seaux
 * contigus et seuls le minimum et le maximum de chaque seau sont gardés (dans l'ordre du temps), plus
 * le premier et le dernier point. Les extrêmes (donc les drawdowns) ne disparaissent jamais.
 */
final class Decimator
{
    public const THRESHOLD = 4000;
    public const BUCKETS = 2000;

    /**
     * @param list<float> $values
     *
     * @return list<int> indices conservés, croissants
     */
    public static function indices(array $values, int $threshold = self::THRESHOLD, int $buckets = self::BUCKETS): array
    {
        $count = count($values);
        if ($count <= $threshold) {
            return $count > 0 ? range(0, $count - 1) : [];
        }

        $keep = [0 => true, $count - 1 => true];
        for ($b = 0; $b < $buckets; ++$b) {
            $from = intdiv($b * $count, $buckets);
            $to = intdiv(($b + 1) * $count, $buckets);
            if ($to <= $from) {
                continue;
            }
            $minAt = $from;
            $maxAt = $from;
            for ($i = $from + 1; $i < $to; ++$i) {
                if ($values[$i] < $values[$minAt]) {
                    $minAt = $i;
                }
                if ($values[$i] > $values[$maxAt]) {
                    $maxAt = $i;
                }
            }
            $keep[$minAt] = true;
            $keep[$maxAt] = true;
        }
        $indices = array_keys($keep);
        sort($indices);

        return $indices;
    }
}
