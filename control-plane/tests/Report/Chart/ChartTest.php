<?php

declare(strict_types=1);

namespace App\Tests\Report\Chart;

use App\Report\Chart\Decimator;
use App\Report\Chart\DrawdownChart;
use App\Report\Chart\HistogramChart;
use App\Report\Chart\LineChart;
use App\Report\Chart\Scale;
use App\Report\Chart\ScatterChart;
use App\Report\Format;
use App\Run\Dto\Histogram;
use App\Run\Dto\TradeRow;
use PHPUnit\Framework\TestCase;

final class ChartTest extends TestCase
{
    private function line(): LineChart
    {
        return new LineChart(new Format());
    }

    public function testScaleMapsDomainToRange(): void
    {
        $scale = new Scale(0.0, 10.0, 100.0, 200.0);

        self::assertSame(100.0, $scale->map(0.0));
        self::assertSame(150.0, $scale->map(5.0));
        self::assertSame(200.0, $scale->map(10.0));
    }

    public function testScaleInvertedRangeForVerticalAxis(): void
    {
        $scale = new Scale(0.0, 10.0, 200.0, 0.0);

        self::assertSame(200.0, $scale->map(0.0));
        self::assertSame(0.0, $scale->map(10.0));
    }

    public function testConstantDomainIsWidenedAndCentred(): void
    {
        $scale = new Scale(300.0, 300.0, 200.0, 0.0);

        self::assertSame(100.0, $scale->map(300.0));
        self::assertLessThan($scale->domainMax, 300.0);
        self::assertGreaterThan($scale->domainMin, 300.0);
    }

    public function testConstantZeroDomain(): void
    {
        $scale = new Scale(0.0, 0.0, 0.0, 100.0);

        self::assertSame(50.0, $scale->map(0.0));
    }

    public function testConstantDomainAwayFromZeroKeepsRelativePadding(): void
    {
        $scale = new Scale(-5.0, -5.0, 0.0, 1.0);

        self::assertSame(-5.05, $scale->domainMin);
        self::assertSame(-4.95, $scale->domainMax);
    }

    public function testTicksAreRoundAndInsideTheDomain(): void
    {
        $scale = new Scale(297.3, 304.1, 0.0, 1.0);
        $ticks = $scale->ticks(4);

        self::assertNotEmpty($ticks);
        foreach ($ticks as $tick) {
            self::assertGreaterThanOrEqual(297.3, $tick);
            self::assertLessThanOrEqual(304.1, $tick);
        }
        self::assertSame([298.0, 300.0, 302.0, 304.0], $ticks);
    }

    public function testLineChartScalesToThePlotArea(): void
    {
        $geometry = $this->line()->build([0, 3_600_000, 7_200_000], [100.0, 110.0, 105.0], fn (float $v): string => (string) $v);

        self::assertFalse($geometry->isEmpty());
        self::assertSame(3, $geometry->pointCount);
        $plot = $geometry->plot;
        // Premier point en bas à gauche de la zone de tracé, le maximum tout en haut.
        self::assertStringStartsWith(sprintf('M%.2F,%.2F', $plot->left, $plot->yBottom()), $geometry->path);
        self::assertStringContainsString(sprintf('L%.2F,%.2F', $plot->left + $plot->plotWidth() / 2, $plot->top), $geometry->path);
        self::assertNull($geometry->areaPath);
        self::assertNotEmpty($geometry->xTicks);
        self::assertNotEmpty($geometry->yTicks);
    }

    public function testLineChartConstantSeries(): void
    {
        $geometry = $this->line()->build([0, 60_000, 120_000], [300.0, 300.0, 300.0], fn (float $v): string => (string) $v);

        $middle = $geometry->plot->top + $geometry->plot->plotHeight() / 2;
        self::assertSame(
            sprintf('M%.2F,%.2FL%.2F,%.2FL%.2F,%.2F', 72.0, $middle, 72.0 + 636.0 / 2, $middle, 72.0 + 636.0, $middle),
            $geometry->path,
        );
    }

    public function testLineChartEmptySeries(): void
    {
        $geometry = $this->line()->build([], [], fn (float $v): string => (string) $v);

        self::assertTrue($geometry->isEmpty());
        self::assertSame('', $geometry->path);
        self::assertSame([], $geometry->xTicks);
    }

    public function testLineChartSinglePoint(): void
    {
        $geometry = $this->line()->build([1_000], [5.0], fn (float $v): string => (string) $v);

        self::assertSame(1, $geometry->pointCount);
        self::assertStringStartsWith('M', $geometry->path);
        self::assertStringNotContainsString('L', $geometry->path);
    }

    public function testDrawdownChartFillsTheAreaUpToZero(): void
    {
        $chart = new DrawdownChart($this->line(), new Format());
        $geometry = $chart->build([0, 60_000, 120_000], [0.0, -0.05, -0.02]);

        self::assertNotNull($geometry->areaPath);
        self::assertStringEndsWith('Z', $geometry->areaPath);
        // La ligne zéro est en haut : le premier point (0 %) est sur le bord haut de la zone.
        self::assertStringStartsWith(sprintf('M%.2F,%.2F', $geometry->plot->left, $geometry->plot->top), $geometry->path);
        self::assertStringContainsString('%', $geometry->yTicks[0]->label);
    }

    public function testDrawdownChartOfAnAllZeroSeriesSpansOneHundredthBelowZero(): void
    {
        $chart = new DrawdownChart($this->line(), new Format());
        $geometry = $chart->build([0, 60_000, 120_000], [0.0, 0.0, 0.0]);

        // Le domaine est [-1 %, 0] : jamais d'axe de -100 % à +100 %, et la ligne zéro reste en haut.
        self::assertStringStartsWith(sprintf('M%.2F,%.2F', $geometry->plot->left, $geometry->plot->top), $geometry->path);
        $labels = array_map(static fn ($tick): string => $tick->label, $geometry->yTicks);
        self::assertSame("\u{2212}1,00\u{00A0}%", $labels[0]);
        self::assertSame("0,00\u{00A0}%", $labels[count($labels) - 1]);
        // Le tick 0 % est au bord haut de la zone de tracé, le tick -1 % au bord bas.
        self::assertEqualsWithDelta($geometry->plot->top, $geometry->yTicks[count($geometry->yTicks) - 1]->pos, 1e-9);
        self::assertEqualsWithDelta($geometry->plot->yBottom(), $geometry->yTicks[0]->pos, 1e-9);
    }

    public function testDrawdownChartTopNeverExceedsZero(): void
    {
        $chart = new DrawdownChart($this->line(), new Format());
        $geometry = $chart->build([0, 60_000, 120_000], [0.0, -0.05, -0.02]);

        foreach ($geometry->yTicks as $tick) {
            self::assertStringNotContainsString('+', $tick->label);
            self::assertGreaterThanOrEqual($geometry->plot->top - 1e-9, $tick->pos);
            self::assertLessThanOrEqual($geometry->plot->yBottom() + 1e-9, $tick->pos);
        }
    }

    public function testDecimatorKeepsEverythingBelowTheThreshold(): void
    {
        self::assertSame([0, 1, 2], Decimator::indices([1.0, 2.0, 3.0]));
        self::assertSame([], Decimator::indices([]));
    }

    public function testDecimatorKeepsExtremesFirstAndLastAbove4000Points(): void
    {
        $values = [];
        for ($i = 0; $i < 10_000; ++$i) {
            $values[] = match ($i) {
                4_321 => -99.0,
                7_777 => 99.0,
                default => sin($i / 50.0) * 10.0,
            };
        }

        $indices = Decimator::indices($values);

        self::assertLessThanOrEqual(2 * Decimator::BUCKETS + 2, count($indices));
        self::assertGreaterThan(2_000, count($indices));
        self::assertSame(0, $indices[0]);
        self::assertSame(9_999, $indices[count($indices) - 1]);
        self::assertContains(4_321, $indices);
        self::assertContains(7_777, $indices);
        self::assertSame($indices, array_values(array_unique($indices)));
        $sorted = $indices;
        sort($sorted);
        self::assertSame($sorted, $indices);
    }

    public function testLineChartDecimatesLargeSeries(): void
    {
        $times = [];
        $values = [];
        for ($i = 0; $i < 20_000; ++$i) {
            $times[] = $i * 60_000;
            $values[] = 300.0 + sin($i / 100.0);
        }

        $geometry = $this->line()->build($times, $values, fn (float $v): string => (string) $v);

        self::assertLessThan(4_100, $geometry->pointCount);
    }

    private function trade(float $mfe, float $mae, float $pnl): TradeRow
    {
        return new TradeRow(
            1,
            0,
            0,
            'taker',
            1.0,
            1.0,
            0.0,
            0.0,
            0,
            0,
            'signal',
            'taker',
            1.0,
            1.0,
            0.0,
            0.0,
            1.0,
            $pnl,
            $mfe,
            $mae,
            null,
            true,
            false,
            'a',
            'b',
            'BTC-USD',
            0,
            0,
            'strategy_exit',
            0,
            0.0,
            null,
            null,
            null,
            null,
            null,
        );
    }

    public function testScatterSeparatesWinnersAndLosersAndKeepsTheOrigin(): void
    {
        $chart = new ScatterChart(new Format());
        $geometry = $chart->build([$this->trade(4.0, -1.0, 2.0), $this->trade(0.5, -3.0, -2.0)]);

        self::assertCount(2, $geometry->points);
        self::assertTrue($geometry->points[0]->winner);
        self::assertFalse($geometry->points[1]->winner);
        // MAE négatif : à gauche de l'origine ; le gagnant (MAE -1) est plus à droite que le perdant (MAE -3).
        self::assertGreaterThan($geometry->points[1]->x, $geometry->points[0]->x);
        // MFE plus grand : plus haut, donc y plus petit.
        self::assertLessThan($geometry->points[1]->y, $geometry->points[0]->y);
    }

    public function testScatterWithoutTrades(): void
    {
        $geometry = (new ScatterChart(new Format()))->build([]);

        self::assertTrue($geometry->isEmpty());
    }

    public function testHistogramBarsAndZeroLine(): void
    {
        $counts = array_fill(0, 30, 0);
        $counts[8] = 2;   // [-1 ; -0,5[
        $counts[12] = 4;  // [1 ; 1,5[
        $histogram = new Histogram(0.5, -5.0, 10.0, $counts, 1, 0);

        $geometry = (new HistogramChart(new Format()))->build($histogram);

        self::assertCount(30, $geometry->bars);
        self::assertTrue($geometry->bars[8]->loss);
        self::assertFalse($geometry->bars[12]->loss);
        self::assertSame(2, $geometry->bars[8]->count);
        // La barre la plus haute atteint le haut de la zone ; une barre vide a une hauteur nulle.
        self::assertEqualsWithDelta($geometry->plot->top, $geometry->bars[12]->y, 0.001);
        self::assertSame(0.0, $geometry->bars[0]->height);
        self::assertNotNull($geometry->zeroX);
        self::assertSame('[1,0 ; 1,5[', $geometry->bars[12]->range);
    }

    public function testHistogramAllZero(): void
    {
        $histogram = new Histogram(0.5, -5.0, 10.0, array_fill(0, 30, 0), 0, 0);

        $geometry = (new HistogramChart(new Format()))->build($histogram);

        self::assertCount(30, $geometry->bars);
        self::assertSame(0.0, $geometry->bars[3]->height);
    }
}
