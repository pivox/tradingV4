<?php

declare(strict_types=1);

namespace App\Tests\Command;

use App\Tests\Support\RunFixture;
use PHPUnit\Framework\Attributes\DataProvider;
use Symfony\Bundle\FrameworkBundle\Console\Application;
use Symfony\Bundle\FrameworkBundle\Test\KernelTestCase;
use Symfony\Component\Console\Tester\CommandTester;

final class Tv4ReportCommandTest extends KernelTestCase
{
    private ?RunFixture $copy = null;

    protected function tearDown(): void
    {
        $this->copy?->cleanup();
        $this->copy = null;
        parent::tearDown();
    }

    private function tester(): CommandTester
    {
        $application = new Application(self::bootKernel());

        return new CommandTester($application->find('tv4:report'));
    }

    private function copy(): RunFixture
    {
        return $this->copy = RunFixture::copy();
    }

    /** Rend le run d'exemple (ou une copie modifiée) et rend le HTML. */
    private function render(?RunFixture $run = null): string
    {
        $run ??= $this->copy();
        $output = $run->path('rapport.html');
        $tester = $this->tester();
        $code = $tester->execute(['run_dir' => $run->directory, '--output' => $output]);
        self::assertSame(0, $code, $tester->getDisplay());
        $html = file_get_contents($output);
        self::assertIsString($html);

        return $html;
    }

    public function testRendersTheFullReport(): void
    {
        $run = $this->copy();
        $output = $run->path('rapport.html');
        $tester = $this->tester();

        $code = $tester->execute(['run_dir' => $run->directory, '--output' => $output]);

        self::assertSame(0, $code, $tester->getDisplay());
        self::assertFileExists($output);
        $html = (string) file_get_contents($output);
        self::assertStringStartsWith('<!DOCTYPE html>', $html);
        self::assertStringContainsString('Run ', $html);
        self::assertStringContainsString('sma_cross', $html);
        self::assertStringContainsString('2021-01-01 04:10 UTC', $html);
        self::assertStringContainsString('2021-01-09 04:10 UTC', $html);
        self::assertSame([], glob($run->directory . '/.report-*'), 'aucun fichier temporaire ne doit rester');
    }

    /**
     * @return iterable<string, array{string}>
     */
    public static function sectionTitles(): iterable
    {
        foreach (['Verdict et marche', 'Résumé', 'Lineage', 'Portes', 'Equity', 'Plis du walk-forward', 'Distribution', 'Pourquoi pas de trade', 'Trades', 'Annexe'] as $title) {
            yield $title => [$title];
        }
    }

    #[DataProvider('sectionTitles')]
    public function testTheTenSectionsArePresent(string $title): void
    {
        self::assertStringContainsString('<h2 id="', $this->render());
        self::assertMatchesRegularExpression('#<h2 id="[a-z-]+">' . preg_quote($title, '#') . '</h2>#u', $this->render());
    }

    public function testSectionsWithoutDataAreMarkedAbsent(): void
    {
        $html = $this->render();

        foreach (['portes', 'plis'] as $id) {
            self::assertMatchesRegularExpression(
                '#<section id="' . $id . '" class="section absent"[^>]*>.*?Absent au jalon 1\.#s',
                $html,
            );
        }
        self::assertStringContainsString('Marche du protocole : absent au jalon 1', $html);
        self::assertStringContainsString('<code>version_hash</code>', $html);
        self::assertStringContainsString('jalon 1.5', $html);
    }

    public function testHashesAreShownVerbatim(): void
    {
        $html = $this->render();

        foreach (
            [
            str_repeat('c0de5a1e', 8),
            'blake3:' . str_repeat('da7a5e70', 8),
            'blake3:' . str_repeat('7e5a1700', 8),
            str_repeat('5e7f1a2b', 8),
            'blake3:' . str_repeat('b1a4e5ee', 8),
            'f4b1c8e0d001',
            '2d8144b7880597b6e6d3dfd63a9a9efae3f533d3',
            'dsh-v1',
            ] as $expected
        ) {
            self::assertStringContainsString($expected, $html);
        }
    }

    public function testReportIsSelfContained(): void
    {
        $html = $this->render();

        self::assertStringNotContainsString('<script', $html);
        self::assertStringNotContainsString('http://', $html);
        self::assertStringNotContainsString('https://', $html);
        self::assertStringNotContainsString('<link', $html);
        self::assertStringNotContainsString('@import', $html);
        self::assertStringNotContainsString('url(', $html);
        self::assertStringContainsString('<style>', $html);
        self::assertStringContainsString('prefers-color-scheme: dark', $html);
    }

    public function testChartsAndDistributionAreRendered(): void
    {
        $html = $this->render();

        self::assertSame(4, substr_count($html, '<svg class="chart"'));
        self::assertStringContainsString('class="line equity"', $html);
        self::assertStringContainsString('class="area dd"', $html);
        self::assertStringContainsString('class="bar win"', $html);
        self::assertStringContainsString('class="bar loss"', $html);
        // Gagnant et perdant se distinguent aussi par la forme (cercle / carré), pas par la couleur seule.
        self::assertStringContainsString('<circle class="pt win"', $html);
        self::assertStringContainsString('<rect class="pt loss"', $html);
        self::assertStringContainsString('Sans les' . '', $html);
        self::assertStringContainsString('Par année de sortie', $html);
        self::assertStringContainsString('Par type de sortie', $html);
        self::assertStringContainsString('<code>trailing_stop</code>', $html);
    }

    public function testFunnelShowsNonZeroReasonsAndTheZeroVolumeCounter(): void
    {
        $html = $this->render();

        self::assertStringContainsString('<code>insufficient_quote</code>', $html);
        self::assertStringContainsString('<code>trend_filter</code>', $html);
        self::assertStringContainsString('<code>bars_zero_volume_blocked</code>', $html);
        self::assertStringNotContainsString('<code>pending_full</code>', $html);
        self::assertStringNotContainsString('<code>limits_expired</code>', $html);
    }

    public function testTagIsEscaped(): void
    {
        $run = $this->copy();
        $run->replace('trades.csv', 'sma_cross_up,sma_cross_down,BTC-USD,1609477260000', '<img src=x onerror=alert(1)>,sma_cross_down,BTC-USD,1609477260000');

        $html = $this->render($run);

        self::assertStringContainsString('&lt;img src=x onerror=alert(1)&gt;', $html);
        self::assertStringNotContainsString('<img', $html);
    }

    public function testDatasetNameAndStrategyAreEscaped(): void
    {
        $run = $this->copy();
        $run->replace('manifest.json', '"dataset": "exemple_fabrique"', '"dataset": "<b>x</b>&\\"y"');

        $html = $this->render($run);

        self::assertStringContainsString('&lt;b&gt;x&lt;/b&gt;&amp;&quot;y', $html);
        self::assertStringNotContainsString('<b>x</b>', $html);
    }

    public function testTwoRendersAreIdenticalByteForByte(): void
    {
        $first = $this->render($this->copy());
        $second = $this->render();

        self::assertSame($first, $second);
        self::assertSame(hash('sha256', $first), hash('sha256', $second));
        self::assertStringNotContainsString(sys_get_temp_dir(), $first, 'aucun chemin absolu dans le HTML');
        self::assertStringNotContainsString(RunFixture::SOURCE, $first);
    }

    public function testDefaultOutputIsReportHtmlInTheRunDirectory(): void
    {
        $run = $this->copy();
        $tester = $this->tester();

        $code = $tester->execute(['run_dir' => $run->directory]);

        self::assertSame(0, $code);
        self::assertFileExists($run->path('report.html'));
        $run->cleanup();
        $this->copy = null;
    }

    public function testVerdictOkAndNegativeExpectancyWordings(): void
    {
        $ok = $this->copy();
        $ok->replace('metrics.json', '"verdict": "rejected_too_few_trades"', '"verdict": "ok"');
        self::assertStringContainsString('espérance nette positive', $this->render($ok));

        $negative = RunFixture::copy();
        try {
            $negative->replace('metrics.json', '"verdict": "rejected_too_few_trades"', '"verdict": "rejected_negative_expectancy"');
            self::assertStringContainsString('soit 0 ou moins', $this->render($negative));
        } finally {
            $negative->cleanup();
        }
    }

    public function testRunWithoutTradesShowsAbsentSections(): void
    {
        $run = $this->copy();
        $header = explode("\n", $run->read('trades.csv'))[0];
        $run->write('trades.csv', $header . "\n");
        $run->replace('metrics.json', '"histogram_r": {', '"histogram_r_ignore": {');
        $run->replace('metrics.json', '"histogram_r_ignore"', '"histogram_r"');

        $html = $this->render($run);

        self::assertMatchesRegularExpression('#<section id="trades" class="section absent"#', $html);
        self::assertStringContainsString('Ce run ne contient aucun trade.', $html);
    }

    public function testInvalidRunExitsWithOneAndNamesTheFile(): void
    {
        $run = $this->copy();
        $run->remove('trades.csv');
        $tester = $this->tester();

        $code = $tester->execute(['run_dir' => $run->directory, '--output' => $run->path('rapport.html')]);

        self::assertSame(1, $code);
        self::assertStringContainsString('trades.csv : fichier absent', $tester->getDisplay());
        self::assertFileDoesNotExist($run->path('rapport.html'));
    }

    public function testInvalidJsonExitsWithOne(): void
    {
        $run = $this->copy();
        $run->write('metrics.json', 'pas du json');
        $tester = $this->tester();

        $code = $tester->execute(['run_dir' => $run->directory, '--output' => $run->path('rapport.html')]);

        self::assertSame(1, $code);
        self::assertStringContainsString('metrics.json : JSON invalide', $tester->getDisplay());
    }

    public function testUnknownSchemaExitsWithOne(): void
    {
        $run = $this->copy();
        $run->replace('metrics.json', '"tv4-metrics-s8"', '"tv4-metrics-s99"');
        $tester = $this->tester();

        $code = $tester->execute(['run_dir' => $run->directory, '--output' => $run->path('rapport.html')]);

        self::assertSame(1, $code);
        self::assertStringContainsString('schéma inconnu', $tester->getDisplay());
    }

    public function testMissingOutputDirectoryExitsWithOne(): void
    {
        $run = $this->copy();
        $tester = $this->tester();

        $code = $tester->execute(['run_dir' => $run->directory, '--output' => $run->path('absent/rapport.html')]);

        self::assertSame(1, $code);
        self::assertStringContainsString('dossier de sortie introuvable', $tester->getDisplay());
    }

    public function testMissingArgumentIsAUsageError(): void
    {
        $tester = $this->tester();

        $this->expectException(\Symfony\Component\Console\Exception\RuntimeException::class);
        $tester->execute([]);
    }
}
