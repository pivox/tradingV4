<?php

declare(strict_types=1);

namespace App\Report\Chart;

use App\Report\Format;

/** Courbe temporelle en coordonnées SVG, calculée en PHP (aucun JS, aucune ressource externe). */
final class LineChart
{
    public function __construct(private readonly Format $format)
    {
    }

    /**
     * @param list<int>   $timesMs
     * @param list<float> $values
     * @param callable(float): string $label étiquette des graduations de l'axe vertical
     * @param float|null $baseline si renseigné, l'aire entre la courbe et cette valeur est aussi calculée
     *                              et le domaine vertical l'inclut
     */
    public function build(array $timesMs, array $values, callable $label, ?float $baseline = null, ?Plot $plot = null): LineGeometry
    {
        $plot ??= new Plot();
        $keep = Decimator::indices($values);
        if ([] === $keep) {
            return new LineGeometry($plot, '', null, [], [], 0);
        }

        $xs = [];
        $ys = [];
        foreach ($keep as $i) {
            $xs[] = $timesMs[$i];
            $ys[] = $values[$i];
        }
        $yMin = min($ys);
        $yMax = max($ys);
        if (null !== $baseline) {
            $yMin = min($yMin, $baseline);
            $yMax = max($yMax, $baseline);
        }

        $xScale = new Scale((float) min($xs), (float) max($xs), $plot->left, $plot->xRight());
        $yScale = new Scale($yMin, $yMax, $plot->yBottom(), $plot->top);

        $points = [];
        foreach ($xs as $k => $x) {
            $points[] = [$xScale->map((float) $x), $yScale->map($ys[$k])];
        }

        $path = '';
        foreach ($points as $k => [$px, $py]) {
            $path .= sprintf('%s%.2F,%.2F', 0 === $k ? 'M' : 'L', $px, $py);
        }

        $areaPath = null;
        if (null !== $baseline) {
            $base = $yScale->map($baseline);
            $first = $points[0];
            $last = $points[count($points) - 1];
            $areaPath = sprintf('M%.2F,%.2F', $first[0], $base) . 'L' . substr($path, 1)
                . sprintf('L%.2F,%.2FZ', $last[0], $base);
        }

        $span = (int) ($xScale->domainMax - $xScale->domainMin);
        $xTicks = [];
        foreach (self::timeTicks($xScale->domainMin, $xScale->domainMax, 6) as $t) {
            $xTicks[] = new Tick($xScale->map($t), $this->format->axisDate((int) $t, $span));
        }
        $yTicks = [];
        foreach ($yScale->ticks(4) as $t) {
            $yTicks[] = new Tick($yScale->map($t), $label($t));
        }

        return new LineGeometry($plot, $path, $areaPath, $xTicks, $yTicks, count($points));
    }

    /**
     * Graduations temporelles alignées sur l'époque UTC (heures, jours, semaines, mois, années).
     *
     * @return list<float>
     */
    public static function timeTicks(float $min, float $max, int $target): array
    {
        $hour = 3_600_000.0;
        $day = 24 * $hour;
        $steps = [
            $hour, 3 * $hour, 6 * $hour, 12 * $hour, $day, 2 * $day, 7 * $day, 14 * $day,
            30 * $day, 91 * $day, 182 * $day, 365 * $day,
        ];
        $chosen = 730 * $day;
        foreach ($steps as $step) {
            if (($max - $min) / $step <= $target) {
                $chosen = $step;
                break;
            }
        }
        $ticks = [];
        $value = ceil($min / $chosen) * $chosen;
        for ($i = 0; $value <= $max && $i < 50; ++$i) {
            $ticks[] = $value;
            $value += $chosen;
        }

        return $ticks;
    }
}
