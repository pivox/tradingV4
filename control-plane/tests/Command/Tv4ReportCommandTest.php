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

    /** Rend le run portefeuille d'exemple (ou une copie modifiée). */
    private function renderPortfolio(?RunFixture $run = null): string
    {
        return $this->render($run ?? $this->copyPortfolio());
    }

    private function copyPortfolio(): RunFixture
    {
        return $this->copy = RunFixture::copyPortfolio();
    }

    public function testS8ReportIsUnchangedByteForByte(): void
    {
        $expected = file_get_contents(__DIR__ . '/../fixtures/rapport-s8-attendu.html');
        self::assertIsString($expected);

        $html = $this->render();

        self::assertSame($expected, $html);
        self::assertStringNotContainsString('id="univers"', $html);
        self::assertStringNotContainsString('id="executabilite"', $html);
        self::assertStringNotContainsString('Instruments</th>', $html);
    }

    /**
     * @return iterable<string, array{string}>
     */
    public static function portfolioSectionTitles(): iterable
    {
        foreach (['Univers et entonnoir par paire', 'Exécutabilité (G0) et G1'] as $title) {
            yield $title => [$title];
        }
    }

    #[DataProvider('portfolioSectionTitles')]
    public function testPortfolioReportHasTheS9Sections(string $title): void
    {
        self::assertMatchesRegularExpression('#<h2 id="[a-z-]+">' . preg_quote($title, '#') . '</h2>#u', $this->renderPortfolio());
    }

    #[DataProvider('sectionTitles')]
    public function testPortfolioReportKeepsTheTenS8Sections(string $title): void
    {
        self::assertMatchesRegularExpression('#<h2 id="[a-z-]+">' . preg_quote($title, '#') . '</h2>#u', $this->renderPortfolio());
    }

    public function testPortfolioHeaderLineageAndFunnelByPair(): void
    {
        $html = $this->renderPortfolio();

        // Forme réelle de tv4 (`data.source = "dataset"`) : la branche « dataset », pas la garde « portefeuille ».
        self::assertStringContainsString('dataset <strong>exemple_portefeuille</strong>', $html);
        self::assertStringNotContainsString('portefeuille <strong>', $html);
        self::assertStringContainsString('(kraken, BTC-USD, ETH-USD, 1m)', $html);
        self::assertStringContainsString('<code>BTC-USD</code> : 12', $html);
        self::assertStringContainsString('aucun : la stratégie ne lit pas le régime', $html);
        self::assertMatchesRegularExpression('#<section id="lineage".*Capital initial \(hors <code>config_hash</code>\)</th><td>300,00#su', $html);
        self::assertStringContainsString('<code>instruments_hash</code> : <code class="hash">blake3:' . str_repeat('1257900c', 8) . '</code>', $html);
        self::assertStringContainsString('relevé le 2026-10-28T12:45:00.123Z', $html);
        self::assertStringContainsString('Hors état <code>live</code> : <code>ETH-USDC</code>', $html);
        self::assertStringContainsString('<code>BTC-USDC</code> : <code>minSz</code> devient <code>0.001</code>, effectif 2026-11-02 06:13 UTC', $html);
        self::assertMatchesRegularExpression('#<h3>Par paire</h3>#', $html);
        self::assertStringContainsString('<th scope="col">Paire</th><th scope="col">Entrée</th>', $html);
        self::assertStringContainsString('<td><code>ETH-USD</code></td>', $html);
        self::assertStringContainsString('<code>gap_forced_exit</code>', $html);
        self::assertStringContainsString('Instrument ETH-USD', $html);
        self::assertStringContainsString('Portefeuille (haché hors capital initial)', $html);
        self::assertStringNotContainsString('une seule paire est jouée', $html);
    }

    public function testMaxPositionsRejectionsCloseTheGlobalFunnel(): void
    {
        $html = $this->renderPortfolio();

        // 15 signaux d'entrée = 10 ordres + 2 refus par motif + 3 refus max_positions (fixture : plafond de 2).
        self::assertMatchesRegularExpression('#Signaux d\'entrée émis</th><td class="num">15</td></tr>\s*<tr class="sub"><th scope="row">dont rejetés : <code>below_minimum</code>.*<tr class="sub"><th scope="row">dont rejetés : <code>max_positions</code></th><td class="num">3</td></tr>\s*<tr><th scope="row">Ordres d\'entrée acceptés</th><td class="num">10</td>#su', $html);
        // 22 signaux émis = 17 acceptés + 5 rejetés, dont 3 refus max_positions.
        self::assertStringContainsString('22 / 17 / 5', $html);
        self::assertMatchesRegularExpression('#<tr class="sub"><th scope="row">rejet : <code>max_positions</code></th><td class="num">3</td></tr>#', $html);
    }

    public function testMaxPositionsLinesAreHiddenWhenZero(): void
    {
        $run = $this->copyPortfolio();
        $run->setJson('metrics.json', ['funnel', 'entry_rejected_max_positions'], 0);
        $run->setJson('metrics.json', ['signals', 'rejected_max_positions'], 0);

        $html = $this->renderPortfolio($run);

        self::assertStringNotContainsString('dont rejetés : <code>max_positions</code>', $html);
        self::assertStringNotContainsString('rejet : <code>max_positions</code>', $html);
    }

    public function testHeaderFallsBackToThePortfolioWordingWithoutDatasetSource(): void
    {
        $run = $this->copyPortfolio();
        $run->unsetJson('manifest.json', ['data', 'source']);
        $run->unsetJson('manifest.json', ['data', 'dataset']);

        $html = $this->renderPortfolio($run);

        self::assertStringContainsString('portefeuille <strong>BTC-USD, ETH-USD</strong>', $html);
        self::assertStringNotContainsString('dataset <strong>', $html);
    }

    public function testPortfolioExecutabilityAndG1(): void
    {
        $html = $this->renderPortfolio();

        self::assertMatchesRegularExpression('#<section id="univers" class="section"#', $html);
        self::assertMatchesRegularExpression('#<section id="executabilite" class="section"#', $html);
        self::assertStringContainsString('<code>rejected_volume</code>', $html);
        self::assertStringContainsString('<code>executable</code>', $html);
        self::assertStringContainsString('<li><code>BTC-USD</code></li>', $html);
        self::assertStringContainsString('ne vaut pas pour l\'exécution réelle', $html);
        self::assertStringContainsString('90,91', $html);
        self::assertStringContainsString('(seuil 95,00', $html);
        self::assertStringContainsString('<code>rejected_cash</code>', $html);
        self::assertStringNotContainsString('ce run est à un autre capital', $html);
    }

    public function testRegimeLineWhenTheStrategyReadsTheRegime(): void
    {
        $run = $this->copyPortfolio();
        $run->replace(
            'metrics.json',
            "\"regime\": null,\n    \"return_total\"",
            "\"regime\": {\"days_bear\": 3, \"days_bull\": 5, \"first_ready_ms\": 1608249600000, \"sma_period\": 200, \"symbol\": \"BTC-USD\", \"tf\": \"1d\"},\n    \"return_total\"",
        );

        $html = $this->renderPortfolio($run);

        self::assertStringContainsString('<code>BTC-USD</code> 1d, SMA 200', $html);
        self::assertStringContainsString('premier jour prêt : 2020-12-18 00:00 UTC', $html);
        self::assertStringContainsString('5 jours haussiers, 3 baissiers', $html);
        self::assertStringNotContainsString('aucun : la stratégie ne lit pas le régime', $html);
    }

    public function testG1AtAnotherCapitalIsFlagged(): void
    {
        $run = $this->copyPortfolio();
        $run->replace('metrics.json', '"at_reference_capital": true', '"at_reference_capital": false');

        $html = $this->renderPortfolio($run);

        self::assertStringContainsString('ce run est à un autre capital', $html);
    }

    public function testSectionsDegradeWhenS9BlocksAreAbsent(): void
    {
        $run = $this->copyPortfolio();
        foreach (['executability', 'g1_executable', 'instruments'] as $key) {
            $run->setJson('metrics.json', [$key], null);
        }
        foreach ([['funnel_by_pair'], ['portfolio'], ['priority'], ['distribution', 'by_pair']] as $path) {
            $run->unsetJson('metrics.json', $path);
        }

        $html = $this->renderPortfolio($run);

        self::assertStringContainsString('Ce run ne porte pas de bloc <code>funnel_by_pair</code>', $html);
        self::assertStringContainsString('Ce run ne porte pas de bloc <code>executability</code>', $html);
        self::assertStringContainsString('Ce run ne porte pas de bloc <code>g1_executable</code>', $html);
        self::assertStringContainsString('<th scope="row">Instruments</th>', $html);
        self::assertStringContainsString('non déclarés. Ce run ne porte pas de bloc <code>instruments</code>', $html);
        self::assertStringContainsString('Ce run ne porte pas de bloc <code>portfolio</code>', $html);
        self::assertStringContainsString('Ce run ne porte pas de bloc <code>priority</code>', $html);
        self::assertStringContainsString('Ce run ne porte pas de bloc <code>distribution.by_pair</code>', $html);
        self::assertStringNotContainsString('une seule paire est jouée', $html);
    }

    /**
     * Chaque bloc obligatoire d'un run s9 a son propre texte d'absence : le job CI `rapport-bout-en-bout` cherche
     * ces motifs dans le rendu d'un vrai run, et le régime, facultatif, n'en a pas.
     *
     * @return iterable<string, array{list<string>, string}>
     */
    public static function mandatoryBlocks(): iterable
    {
        yield 'executability' => [['executability'], 'executability'];
        yield 'g1_executable' => [['g1_executable'], 'g1_executable'];
        yield 'funnel_by_pair' => [['funnel_by_pair'], 'funnel_by_pair'];
        yield 'portfolio' => [['portfolio'], 'portfolio'];
        yield 'priority' => [['priority'], 'priority'];
        yield 'instruments' => [['instruments'], 'instruments'];
        yield 'distribution.by_pair' => [['distribution', 'by_pair'], 'distribution.by_pair'];
    }

    /**
     * @param list<string> $path
     */
    #[DataProvider('mandatoryBlocks')]
    public function testEachMandatoryBlockHasItsOwnAbsenceNote(array $path, string $name): void
    {
        $present = $this->renderPortfolio();
        self::assertStringNotContainsString('Ce run ne porte pas de bloc', $present);

        $run = $this->copyPortfolio();
        $run->unsetJson('metrics.json', $path);
        $html = $this->renderPortfolio($run);

        self::assertStringContainsString('Ce run ne porte pas de bloc <code>' . $name . '</code>', $html);
        self::assertSame(1, substr_count($html, 'Ce run ne porte pas de bloc'));
    }

    public function testNotApplicableVerdictsAndNoUpcomingChanges(): void
    {
        $run = $this->copyPortfolio();
        $run->replace('metrics.json', '"verdict": "fail"', '"verdict": "not_applicable"');
        $run->setJson('metrics.json', ['instruments'], [
            'fetched_at' => null,
            'file' => null,
            'instruments_hash' => null,
            'not_live' => [],
            'source' => 'toml',
            'upcoming_changes' => [],
        ]);
        $run->setJson('manifest.json', ['instruments_hash'], null);
        $run->setJson('manifest.json', ['instruments_file'], null);

        $html = $this->renderPortfolio($run);

        self::assertStringContainsString('source <code>toml</code>', $html);
        self::assertStringContainsString('<code>not_applicable</code>', $html);
        self::assertStringNotContainsString('<code>instruments_hash</code>', $html);
        self::assertStringNotContainsString('Hors état', $html);
        self::assertMatchesRegularExpression('#Changements à venir annoncés par OKX \(non appliqués au backtest\) :\s*aucun#', $html);
    }

    public function testPortfolioTextsAreEscaped(): void
    {
        $run = $this->copyPortfolio();
        $run->replace('metrics.json', '"reason": "rejected_volume"', '"reason": "<script>alert(1)</script>"');

        $html = $this->renderPortfolio($run);

        self::assertStringContainsString('&lt;script&gt;alert(1)&lt;/script&gt;', $html);
        self::assertStringNotContainsString('<script', $html);
    }

    public function testPortfolioReportIsSelfContainedAndDeterministic(): void
    {
        $first = $this->renderPortfolio();
        $second = $this->renderPortfolio();

        self::assertSame($first, $second);
        self::assertStringNotContainsString('http://', $first);
        self::assertStringNotContainsString('https://', $first);
        self::assertStringNotContainsString(RunFixture::PORTFOLIO_SOURCE, $first);
    }
}
