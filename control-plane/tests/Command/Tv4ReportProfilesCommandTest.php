<?php

declare(strict_types=1);

namespace App\Tests\Command;

use App\Tests\Support\RunFixture;
use PHPUnit\Framework\Attributes\DataProvider;
use Symfony\Bundle\FrameworkBundle\Console\Application;
use Symfony\Bundle\FrameworkBundle\Test\KernelTestCase;
use Symfony\Component\Console\Tester\CommandTester;

/**
 * Rendu des runs s10 et des dossiers de profils (docs/specs/s10.md §13), et garantie que les rendus s8 et s9
 * ne changent pas d'un octet.
 */
final class Tv4ReportProfilesCommandTest extends KernelTestCase
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
        return new CommandTester((new Application(self::bootKernel()))->find('tv4:report'));
    }

    private function copy(): RunFixture
    {
        return $this->copy = RunFixture::copyProfiles();
    }

    /** Rend `$directory` (ou un sous-dossier de la copie) et rend le HTML. */
    private function render(string $directory): string
    {
        $output = sys_get_temp_dir() . '/tv4-rapport-' . bin2hex(random_bytes(6)) . '.html';
        $tester = $this->tester();
        $code = $tester->execute(['run_dir' => $directory, '--output' => $output]);
        self::assertSame(0, $code, $tester->getDisplay());
        $html = file_get_contents($output);
        unlink($output);
        self::assertIsString($html);

        return $html;
    }

    private function renderProfiles(?RunFixture $run = null): string
    {
        return $this->render(($run ?? $this->copy())->directory);
    }

    public function testS9ReportIsUnchangedByteForByte(): void
    {
        $expected = file_get_contents(__DIR__ . '/../fixtures/rapport-s9-attendu.html');
        self::assertIsString($expected);

        $html = $this->render(RunFixture::PORTFOLIO_SOURCE);

        self::assertSame($expected, $html);
        self::assertStringNotContainsString('id="couts"', $html);
        self::assertStringNotContainsString('id="profils"', $html);
        self::assertStringNotContainsString('Spread</th>', $html);
    }

    public function testS8ReportIsUnchangedByteForByte(): void
    {
        $expected = file_get_contents(__DIR__ . '/../fixtures/rapport-s8-attendu.html');
        self::assertIsString($expected);

        self::assertSame($expected, $this->render(RunFixture::SOURCE));
    }

    public function testTheProfilesReportStartsWithTheProfilesSection(): void
    {
        $html = $this->renderProfiles();

        self::assertMatchesRegularExpression('#<h2 id="profils-titre">Profils de coûts</h2>#u', $html);
        self::assertLessThan(strpos($html, '<article class="run">'), strpos($html, 'id="profils"'));
        self::assertStringContainsString('<title>Profils de coûts · Run ', $html);
        // La fiche qui suit est celle du profil baseline, avec sa section Coûts.
        self::assertStringContainsString('Profil de coûts</th><td><code>baseline</code>', $html);
        self::assertMatchesRegularExpression('#<h2 id="couts-titre">Coûts</h2>#u', $html);
        foreach (['Verdict et marche', 'Résumé', 'Lineage', 'Equity', 'Distribution', 'Trades', 'Annexe'] as $title) {
            self::assertMatchesRegularExpression('#<h2 id="[a-z-]+">' . preg_quote($title, '#') . '</h2>#u', $html);
        }
        self::assertSame(1, substr_count($html, 'id="profils"'));
        self::assertSame(1, substr_count($html, 'id="couts"'));
    }

    public function testTheFillDependentLabelIsRenderedInEvidence(): void
    {
        $html = $this->renderProfiles();

        self::assertStringContainsString('<p class="badge label-fill_dependent">Étiquette : <strong>dépend du fill</strong>', $html);
        self::assertStringContainsString('<p class="callout fill-dependent"><strong>Dépend du fill.</strong>', $html);
        self::assertStringContainsString("70,00\u{00A0}%", $html);
        self::assertStringContainsString('.badge.label-fill_dependent', $html);
    }

    /**
     * @return iterable<string, array{string, string, string, bool}>
     */
    public static function labels(): iterable
    {
        yield 'passes_costs' => ['passes_costs', 'tient les coûts', 'la stratégie tient les coûts', false];
        yield 'cost_fragile' => ['cost_fragile', 'fragile aux coûts', 'ne survit pas au profil de coûts adverse', false];
        yield 'negative' => ['negative', 'négatif', 'Baseline et adverse non positifs, sans profil maker positif', false];
        yield 'indeterminate' => ['indeterminate', 'indéterminé', 'aucune conclusion sur les coûts', false];
    }

    #[DataProvider('labels')]
    public function testEachLabelIsRenderedFromTheFile(string $label, string $labelFr, string $sentence, bool $callout): void
    {
        $run = $this->copy();
        $run->setJson('profiles.json', ['label'], $label);
        $run->setJson('profiles.json', ['label_fr'], $labelFr);

        $html = $this->renderProfiles($run);

        self::assertStringContainsString('<p class="badge label-' . $label . '">Étiquette : <strong>' . $labelFr . '</strong>', $html);
        self::assertStringContainsString($sentence, $html);
        self::assertSame($callout, str_contains($html, 'class="callout fill-dependent"'));
        self::assertStringNotContainsString('Dépend du fill.', $html);
    }

    public function testTheMakerStatusAndItsShareAreShown(): void
    {
        $run = $this->copy();
        $run->setJson('profiles.json', ['maker', 'status'], 'positive_mostly_taker');
        $run->setJson('profiles.json', ['maker', 'maker_entry_maker_share'], 0.3);

        $html = $this->renderProfiles($run);

        self::assertStringContainsString('<code>positive_mostly_taker</code> : positif, mais entrées surtout prises en taker', $html);
        self::assertStringContainsString("30,00\u{00A0}% (seuil 50,00\u{00A0}%)", $html);
    }

    public function testTheProfilesTableAndTheCrossedGates(): void
    {
        $html = $this->renderProfiles();

        foreach (['baseline', 'adverse', 'maker'] as $name) {
            self::assertStringContainsString('<th scope="row"><code>' . $name . '</code></th>', $html);
        }
        self::assertStringContainsString('<tr class="gate-off">', $html);
        self::assertSame(1, substr_count($html, '<tr class="gate-off">'), 'seul le profil informatif est estompé');
        self::assertStringContainsString('Portes croisées (baseline et adverse)', $html);
        self::assertStringContainsString('Gain brut médian ≥ 2 × coût baseline', $html);
        self::assertStringContainsString('Baseline et adverse positifs', $html);
        self::assertStringContainsString('<th scope="col">Coût ≤ 1/3 du brut</th>', $html);
        self::assertStringContainsString('<th scope="col" class="num">Seuil (R)</th>', $html);
        self::assertStringContainsString('≤ 0,10', $html);
        self::assertStringContainsString('<code>rejected_negative_expectancy</code>', $html);
        self::assertStringContainsString('Stress calibré</th><td>non : la probabilité de stress est provisoire', $html);
    }

    public function testASubDirectoryRendersOnlyTheS10Sheet(): void
    {
        $html = $this->render(RunFixture::PROFILES_SOURCE . '/maker');

        self::assertStringNotContainsString('id="profils"', $html);
        self::assertStringNotContainsString('<title>Profils de coûts', $html);
        self::assertStringContainsString('<title>Run ', $html);
        self::assertMatchesRegularExpression('#<h2 id="couts-titre">Coûts</h2>#u', $html);
        self::assertStringContainsString("<code>maker</code>, TTL 15 min, fill 45,00\u{00A0}%", $html);
        // Le profil maker du dossier de profils : une ligne de lineage dit son profil.
        self::assertStringContainsString('Profil de coûts</th><td><code>maker</code> (profil informatif, hors portes)', $html);
    }

    public function testTheCostsSectionShowsLegsArithmeticAndCounters(): void
    {
        $html = $this->render(RunFixture::PROFILES_SOURCE . '/baseline');

        self::assertStringContainsString('Paramètres par jambe', $html);
        self::assertStringContainsString('<code>BTC-USD</code>', $html);
        self::assertStringContainsString('major</code> (liste)', $html);
        self::assertStringContainsString('0,50 (défaut)', $html);
        self::assertStringContainsString('20,625', $html);
        self::assertStringContainsString('Arithmétique du coût (04 §3.3)', $html);
        self::assertStringContainsString('TF de décision</th><td><code>15m</code> (hors TF du protocole)', $html);
        self::assertStringContainsString('≤ 0,10 R', $html);
        self::assertStringContainsString('dont frais', $html);
        self::assertStringContainsString('dont slippage', $html);
        self::assertStringContainsString('dont spread', $html);
        self::assertStringContainsString('Compteurs de coûts', $html);
        self::assertStringContainsString('<th scope="row">Total</th>', $html);
        self::assertStringContainsString('<code>cost_defaults_unmeasured</code>', $html);
        self::assertStringContainsString('<code>stop_stress_uncalibrated</code>', $html);
        // Le slippage scalaire n'existe plus en s10 ; la grille vient de costs.fee_grid.
        self::assertStringContainsString('<tr><th scope="row">Grille de frais</th><td>okx_regular</td></tr>', $html);
        self::assertStringNotContainsString('<th scope="row">Slippage</th>', $html);
        // Colonne Spread du tableau des trades.
        self::assertStringContainsString('<th scope="col" class="num">Spread</th>', $html);
        self::assertStringNotContainsString('Ce run ne porte pas de bloc', $html);
    }

    public function testTheUncalibratedStressNoteDisappearsOnceCalibrated(): void
    {
        $run = $this->copy();
        $baseline = $run->path('baseline');
        $before = $this->render($baseline);
        self::assertStringContainsString('Stress non calibré.', $before);

        $run->setJson('baseline/metrics.json', ['costs', 'stress_calibrated'], true);
        $run->setJson('baseline/metrics.json', ['costs', 'stress_calibration_ref'], 'blake3:' . str_repeat('ab', 32));
        $after = $this->render($baseline);

        self::assertStringNotContainsString('Stress non calibré.', $after);
        self::assertStringContainsString('Stress calibré</th><td>oui · mesure <code class="hash">blake3:' . str_repeat('ab', 32) . '</code>', $after);
    }

    public function testAFallbackClassAndMeasuredSourcesAreLabelled(): void
    {
        $run = $this->copy();
        $run->setJson('baseline/metrics.json', ['costs', 'legs', '1', 'class'], 'beyond');
        $run->setJson('baseline/metrics.json', ['costs', 'legs', '1', 'class_source'], 'fallback');
        $run->setJson('baseline/metrics.json', ['costs', 'legs', '1', 'spread_source'], 'measured');
        $run->setJson('baseline/metrics.json', ['costs', 'legs', '1', 'measured_p95_half_spread_bps'], 12.5);

        $html = $this->render($run->path('baseline'));

        self::assertStringContainsString('beyond</code> (repli, classe la plus chère)', $html);
        self::assertStringContainsString('(mesuré)', $html);
        self::assertStringContainsString('Spread p95 mesuré pour <code>ETH-USD</code> (non appliqué) : 12,50 bp.', $html);
    }

    public function testNullArithmeticValuesRenderAsDashes(): void
    {
        $run = $this->copy();
        foreach (['cost_r_mean', 'cost_r_median', 'cost_r_pass', 'gross_expectancy', 'cost_to_gross', 'hurdle_annual'] as $key) {
            $run->setJson('baseline/metrics.json', ['cost_arithmetic', $key], null);
        }

        $html = $this->render($run->path('baseline'));

        self::assertStringContainsString('Seuil respecté</th><td>—</td>', $html);
        self::assertStringContainsString('Hurdle annuel</th><td class="num">—</td>', $html);
    }

    public function testAnS10RunWithoutItsBlocksExitsWithOne(): void
    {
        $run = $this->copy();
        $run->unsetJson('baseline/metrics.json', ['cost_arithmetic']);
        $tester = $this->tester();

        $code = $tester->execute(['run_dir' => $run->path('baseline'), '--output' => $run->path('rapport.html')]);

        self::assertSame(1, $code);
        self::assertStringContainsString('metrics.json : clé absente à cost_arithmetic', $tester->getDisplay());
    }

    public function testAnInvalidProfilesDirectoryExitsWithOne(): void
    {
        $run = $this->copy();
        $run->replace('profiles.json', '"tv4-profiles-v1"', '"tv4-profiles-v0"');
        $tester = $this->tester();

        $code = $tester->execute(['run_dir' => $run->directory, '--output' => $run->path('rapport.html')]);

        self::assertSame(1, $code);
        self::assertStringContainsString('profiles.json : schéma inconnu', $tester->getDisplay());
        self::assertFileDoesNotExist($run->path('rapport.html'));
    }

    public function testDefaultOutputOfAProfilesDirectoryIsReportHtmlInThatDirectory(): void
    {
        $run = $this->copy();
        $tester = $this->tester();

        $code = $tester->execute(['run_dir' => $run->directory]);

        self::assertSame(0, $code, $tester->getDisplay());
        self::assertFileExists($run->path('report.html'));
        self::assertFileDoesNotExist($run->path('baseline/report.html'));
    }

    public function testProfilesAndS10ReportsAreSelfContainedAndDeterministic(): void
    {
        foreach ([RunFixture::PROFILES_SOURCE, RunFixture::PROFILES_SOURCE . '/adverse'] as $directory) {
            $first = $this->render($directory);

            self::assertSame($first, $this->render($directory));
            self::assertStringNotContainsString('<script', $first);
            self::assertDoesNotMatchRegularExpression('#(src|href)="https?://#', $first);
            self::assertStringNotContainsString('/tmp/', $first);
        }
    }

    public function testTextsOfTheProfilesFileAreEscaped(): void
    {
        $run = $this->copy();
        $run->setJson('profiles.json', ['label_fr'], '<b>"x"</b>');

        $html = $this->renderProfiles($run);

        self::assertStringContainsString('&lt;b&gt;&quot;x&quot;&lt;/b&gt;', $html);
        self::assertStringNotContainsString('<b>"x"</b>', $html);
    }
}
