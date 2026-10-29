<?php

declare(strict_types=1);

namespace App\Tests\Command;

use App\Tests\Support\RunFixture;
use Symfony\Bundle\FrameworkBundle\Console\Application;
use Symfony\Bundle\FrameworkBundle\Test\KernelTestCase;
use Symfony\Component\Console\Tester\CommandTester;

/**
 * Rendu des runs s11, des dossiers de lignes et des lignes de profils (docs/specs/s11.md §10), et garantie que les
 * rendus s8, s9 et s10 ne changent pas d'un octet. `run-recherche/` et `run-lignes/` sont des sorties réelles de `tv4` ;
 * `run-recherche-trimestriel/`, `run-lignes-garde/` et `run-lignes-profils/` sont écrits à la main (univers
 * trimestriel, trade purgé, garde levée, profils de coûts).
 */
final class Tv4ReportResearchCommandTest extends KernelTestCase
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

    /** Rend `$directory` et rend le HTML. */
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

    public function testS8S9AndS10ReportsAreUnchangedByteForByte(): void
    {
        foreach (
            [
                'rapport-s8-attendu.html' => RunFixture::SOURCE,
                'rapport-s9-attendu.html' => RunFixture::PORTFOLIO_SOURCE,
                'rapport-s10-attendu.html' => RunFixture::PROFILES_SOURCE,
            ] as $expectedFile => $directory
        ) {
            $expected = file_get_contents(__DIR__ . '/../fixtures/' . $expectedFile);
            self::assertIsString($expected);

            $html = $this->render($directory);

            self::assertSame($expected, $html, $expectedFile);
            self::assertStringNotContainsString('id="recherche"', $html, $expectedFile);
            self::assertStringNotContainsString('id="lignes"', $html, $expectedFile);
            self::assertStringNotContainsString('callout-garde', $html, $expectedFile);
        }
    }

    public function testTheRealResearchRunIsRenderedWithItsResearchSection(): void
    {
        $html = $this->render(RunFixture::RESEARCH_SOURCE);

        self::assertSame(1, substr_count($html, 'id="recherche"'));
        self::assertLessThan(strpos($html, 'id="verdict"'), strpos($html, 'id="recherche"'));
        self::assertStringContainsString('<code>DEV</code>', $html);
        self::assertStringContainsString('2018-01-01 00:00 UTC → 2023-01-01 00:00 UTC', $html);
        self::assertStringContainsString('aucun (univers fixe)', $html);
        self::assertStringNotContainsString('Trimestres de l\'univers', $html);
        self::assertStringNotContainsString('<p class="callout-garde">', $html);
        self::assertStringNotContainsString('id="lignes"', $html);
        self::assertStringNotContainsString('id="profils"', $html);
        self::assertStringNotContainsString('Ce run ne porte pas de bloc', $html);
    }

    public function testTheQuarterlyResearchRunHasAResearchSectionBeforeTheVerdict(): void
    {
        $html = $this->render(RunFixture::RESEARCH_QUARTERLY_SOURCE);

        self::assertSame(1, substr_count($html, 'id="recherche"'));
        self::assertMatchesRegularExpression('#<h2 id="recherche-titre">Recherche</h2>#u', $html);
        self::assertGreaterThan(strpos($html, '<article class="run">'), strpos($html, 'id="recherche"'));
        self::assertLessThan(strpos($html, 'id="verdict"'), strpos($html, 'id="recherche"'));
        self::assertStringContainsString('Les métriques de trades portent sur les trades conservés', $html);
        self::assertStringContainsString('<code>FORWARD</code>', $html);
        self::assertStringContainsString('2026-07-01 00:00 UTC → 2026-07-09 04:10 UTC', $html);
        self::assertStringContainsString('Purge des trades (<code>end_of_window_v1</code>)', $html);
        self::assertStringContainsString('Refus d\'entrée de la recherche', $html);
        self::assertStringContainsString('<code>2026-Q3</code>', $html);
        self::assertStringContainsString('Exclusions qualité par paire', $html);
        self::assertStringContainsString('<code>research.identity_hash</code>', $html);
        self::assertStringContainsString('Entrées de la recherche (<code>manifest.research_inputs</code>)', $html);
        // Run sans garde levée, sans lignes ni profils.
        self::assertStringNotContainsString('<div class="callout-garde">', $html);
        self::assertStringNotContainsString('<p class="callout-garde">', $html);
        self::assertStringNotContainsString('id="lignes"', $html);
        self::assertStringNotContainsString('id="profils"', $html);
        self::assertStringNotContainsString('id="couts"', $html);
        self::assertStringNotContainsString('Ce run ne porte pas de bloc', $html);
        // Le lineage donne le bloc et l'identité de recherche.
        self::assertStringContainsString('Identité de recherche (<code>research.identity_hash</code>)', $html);
        self::assertStringContainsString('<title>Run ', $html);
    }

    /** Texte d'une ligne `<tr>` dont l'en-tête contient `$label`, valeur numérique de la cellule. */
    private function rowValue(string $html, string $label): int
    {
        $row = $this->between($html, '<th scope="row">' . $label . '</th><td class="num">', '</td>');

        return (int) $row;
    }

    /** Sous-chaîne de `$html` entre la première occurrence de `$start` et la suivante de `$end`. */
    private function between(string $html, string $start, string $end): string
    {
        $from = strpos($html, $start);
        self::assertIsInt($from, $start);
        $from += strlen($start);
        $to = strpos($html, $end, $from);
        self::assertIsInt($to, $end);

        return substr($html, $from, $to - $from);
    }

    /** Section `<section id="$id" ...>` jusqu'à sa fermeture. */
    private function section(string $html, string $id): string
    {
        return $this->between($html, '<section id="' . $id . '"', '</section>');
    }

    public function testTheResearchFunnelClosesInTheWhyNoTradeAndPerPairSections(): void
    {
        $html = $this->render(RunFixture::RESEARCH_QUARTERLY_SOURCE);

        $why = $this->section($html, 'pourquoi');
        $emitted = $this->rowValue($why, 'Signaux d\'entrée émis');
        $refused = $this->rowValue($why, 'dont refusés par la recherche : <code>outside_decision_window</code>');
        self::assertSame(5, $refused);
        self::assertStringNotContainsString('<code>not_in_universe</code>', $why);
        self::assertStringNotContainsString('<code>excluded_quality</code>', $why);
        $rejected = 0;
        foreach (['below_minimum', 'insufficient_quote', 'max_positions'] as $reason) {
            $rejected += $this->rowValue($why, 'dont rejetés : <code>' . $reason . '</code>');
        }
        self::assertSame(20, $emitted);
        self::assertSame($emitted, $this->rowValue($why, 'Ordres d\'entrée acceptés') + $rejected + $refused);

        $universe = $this->section($html, 'univers');
        self::assertStringContainsString('refus de recherche</th>', $universe);
        self::assertSame(1, preg_match('#<code>BTC-USD</code></th>(?:(?!</tr>).)*<td class="num">3</td>\s*</tr>#su', $universe));
        self::assertSame(1, preg_match('#<code>ETH-USD</code></th>(?:(?!</tr>).)*<td class="num">2</td>\s*</tr>#su', $universe));
    }

    public function testThePurgedTradeIsFlaggedAndLeftOutOfTheScatter(): void
    {
        $html = $this->render(RunFixture::RESEARCH_QUARTERLY_SOURCE);

        self::assertStringContainsString('10 trades, dont 1 purgé (hors métriques de trades)', $html);
        self::assertStringContainsString('<code>end_of_window</code> (purgé)', $html);
        self::assertStringNotContainsString('<code>end_of_window</code> (forcée)', $html);
        $svg = $this->between($html, '<svg class="chart" viewBox="0 0 720 280" role="img" aria-labelledby="scatter-titre">', '</svg>');
        self::assertSame(9, preg_match_all('#<(?:circle|rect) class="pt #', $svg));
    }

    public function testAFixedSymbolsResearchRunHasNoEmptyResearchInputsTable(): void
    {
        $html = $this->render(RunFixture::RESEARCH_SOURCE);

        self::assertStringNotContainsString('Entrées de la recherche (<code>manifest.research_inputs</code>)', $html);
        self::assertStringContainsString('id="recherche"', $html);
    }

    public function testTheResearchRunRendersInTheSameWayTwice(): void
    {
        foreach ([RunFixture::RESEARCH_SOURCE, RunFixture::RESEARCH_QUARTERLY_SOURCE] as $directory) {
            self::assertSame($this->render($directory), $this->render($directory), $directory);
        }
    }

    public function testTheReportIsSelfContained(): void
    {
        foreach (
            [
                RunFixture::RESEARCH_SOURCE,
                RunFixture::RESEARCH_QUARTERLY_SOURCE,
                RunFixture::LINES_SOURCE,
                RunFixture::LINES_GUARD_SOURCE,
                RunFixture::LINES_PROFILES_SOURCE,
            ] as $directory
        ) {
            $html = $this->render($directory);

            self::assertStringNotContainsString('<script', $html, $directory);
            self::assertStringNotContainsString('http://', $html, $directory);
            self::assertStringNotContainsString('https://', $html, $directory);
        }
    }

    public function testAnUnguardedResearchRunShowsTheGuardBanner(): void
    {
        $this->copy = RunFixture::copy(RunFixture::RESEARCH_SOURCE);
        $this->copy->setJson('metrics.json', ['research', 'guard'], 'unguarded');

        $html = $this->render($this->copy->directory);

        self::assertStringContainsString('<p class="callout-garde"><strong>Lecture protégée sans garde-fou.</strong>', $html);
        self::assertStringContainsString('--unguarded-protected-read', $html);
        self::assertStringContainsString('.callout-garde { border-left', $html);
    }

    public function testAFixedUniverseResearchRunIsRendered(): void
    {
        $this->copy = RunFixture::copy(RunFixture::RESEARCH_QUARTERLY_SOURCE);
        $this->copy->setJson('metrics.json', ['research', 'universe'], ['mode' => 'fixed']);
        $this->copy->setJson('metrics.json', ['research', 'identity', 'universe_window_hash'], null);

        $html = $this->render($this->copy->directory);

        self::assertStringContainsString('(jambes données)', $html);
        self::assertStringNotContainsString('Trimestres de l\'univers', $html);
        self::assertStringContainsString('aucun (univers fixe)', $html);
    }

    public function testTheRealLinesReportStartsWithTheTwoLinesSection(): void
    {
        $html = $this->render(RunFixture::LINES_SOURCE);

        self::assertMatchesRegularExpression('#<h2 id="lignes-titre">Deux lignes</h2>#u', $html);
        self::assertSame(1, substr_count($html, 'id="lignes"'));
        self::assertSame(1, substr_count($html, 'id="recherche"'));
        self::assertLessThan(strpos($html, '<article class="run">'), strpos($html, 'id="lignes"'));
        self::assertStringContainsString('Survivance résiduelle (<code>full</code> − <code>okx_usdc</code>)', $html);
        self::assertStringContainsString('<code>DEV</code>', $html);
        // Run joué sans option cachée : aucune garde levée, donc aucun bandeau.
        self::assertStringNotContainsString('<p class="callout-garde">', $html);
        self::assertStringNotContainsString('id="profils"', $html);
        self::assertStringNotContainsString('Ce run ne porte pas de bloc', $html);
        $expected = json_decode((string) file_get_contents(RunFixture::LINES_SOURCE . '/full/manifest.json'), true, 512, JSON_THROW_ON_ERROR);
        self::assertIsArray($expected);
        self::assertIsString($expected['run_id']);
        self::assertStringContainsString('<h1>Run <code class="hash">' . $expected['run_id'] . '</code></h1>', $html);
    }

    public function testTheGuardedLinesReportStartsWithTheTwoLinesSection(): void
    {
        $html = $this->render(RunFixture::LINES_GUARD_SOURCE);

        self::assertMatchesRegularExpression('#<h2 id="lignes-titre">Deux lignes</h2>#u', $html);
        self::assertSame(1, substr_count($html, 'id="lignes"'));
        self::assertLessThan(strpos($html, '<article class="run">'), strpos($html, 'id="lignes"'));
        self::assertStringContainsString('<title>Deux lignes · Run ', $html);
        self::assertStringContainsString('Univers complet (<code>full</code>)', $html);
        self::assertStringContainsString('Sous-ensemble OKX USDC (<code>okx_usdc</code>)', $html);
        self::assertStringContainsString('Survivance résiduelle (<code>full</code> − <code>okx_usdc</code>)', $html);
        self::assertStringContainsString('<code>okx_usdc_tradable</code>', $html);
        self::assertStringContainsString('<code>lines_id</code>', $html);
        self::assertStringContainsString('tr.ligne-residuelle', $html);
        // Les runs de ce dossier sont joués sans garde-fou : le bandeau apparaît en tête, puis dans la fiche.
        self::assertSame(2, substr_count($html, '<p class="callout-garde">'));
        // La fiche qui suit est celle de la ligne full, avec sa section Recherche.
        self::assertSame(1, substr_count($html, 'id="recherche"'));
        self::assertStringContainsString('<code>VALIDATION</code>', $html);
        self::assertStringContainsString('ligne <code>full</code>', $html);
        self::assertStringNotContainsString('id="profils"', $html);
        self::assertStringNotContainsString('Ce run ne porte pas de bloc', $html);
        $expected = json_decode((string) file_get_contents(RunFixture::LINES_GUARD_SOURCE . '/full/manifest.json'), true, 512, JSON_THROW_ON_ERROR);
        self::assertIsArray($expected);
        self::assertIsString($expected['run_id']);
        self::assertStringContainsString('<h1>Run <code class="hash">' . $expected['run_id'] . '</code></h1>', $html);
    }

    public function testTheLinesReportOfProfilesAddsTheProfilesSectionAndTheBaselineSheet(): void
    {
        $html = $this->render(RunFixture::LINES_PROFILES_SOURCE);

        self::assertSame(1, substr_count($html, 'id="lignes"'));
        self::assertSame(1, substr_count($html, 'id="profils"'));
        self::assertSame(1, substr_count($html, 'id="recherche"'));
        self::assertSame(1, substr_count($html, 'id="couts"'));
        $lines = strpos($html, 'id="lignes"');
        $profiles = strpos($html, 'id="profils"');
        $article = strpos($html, '<article class="run">');
        self::assertIsInt($lines);
        self::assertIsInt($profiles);
        self::assertIsInt($article);
        self::assertLessThan($profiles, $lines);
        self::assertLessThan($article, $profiles);
        self::assertStringContainsString('<title>Deux lignes · Profils de coûts · Run ', $html);
        foreach (['baseline', 'adverse', 'maker'] as $profile) {
            self::assertStringContainsString('Profil <code>' . $profile . '</code>', $html);
            self::assertStringContainsString('<code>config_hash</code> ' . $profile, $html);
        }
        self::assertStringContainsString('profil <strong>baseline</strong>', $html);
        self::assertStringContainsString('dossier de profils', $html);
        self::assertStringContainsString('.profils-page', $html);
        self::assertStringContainsString('.lignes-page', $html);
        self::assertStringContainsString('Profil de coûts</th><td><code>baseline</code>', $html);
        self::assertStringNotContainsString('Ce run ne porte pas de bloc', $html);
    }

    public function testTheProfilesSubdirectoryOfALineRendersItsOwnSheet(): void
    {
        $html = $this->render(RunFixture::LINES_PROFILES_SOURCE . '/okx_usdc');

        self::assertSame(1, substr_count($html, 'id="profils"'));
        self::assertSame(1, substr_count($html, 'id="recherche"'));
        self::assertStringContainsString('<code>okx_usdc</code> : jambes tradables sur OKX en USDC', $html);
        self::assertStringNotContainsString('id="lignes"', $html);
    }

    public function testAnInvalidLinesDirectoryExitsWithOne(): void
    {
        $this->copy = RunFixture::copyLines();
        $this->copy->remove('okx_usdc/metrics.json');
        $tester = $this->tester();

        $code = $tester->execute(['run_dir' => $this->copy->directory, '--output' => $this->copy->path('rapport.html')]);

        self::assertSame(1, $code);
        self::assertStringContainsString('Run invalide', $tester->getDisplay());
        self::assertStringContainsString('okx_usdc/', $tester->getDisplay());
        self::assertFileDoesNotExist($this->copy->path('rapport.html'));
    }

    public function testALinesFileTakesPrecedenceOverAProfilesFile(): void
    {
        $this->copy = RunFixture::copyLines(RunFixture::LINES_PROFILES_SOURCE);
        // Un dossier de lignes qui contiendrait aussi profiles.json reste un dossier de lignes.
        $this->copy->write('profiles.json', $this->copy->read('full/profiles.json'));

        $html = $this->render($this->copy->directory);

        self::assertSame(1, substr_count($html, 'id="lignes"'));
    }
}
