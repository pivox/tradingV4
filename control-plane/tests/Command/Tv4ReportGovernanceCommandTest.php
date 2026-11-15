<?php

declare(strict_types=1);

namespace App\Tests\Command;

use App\Tests\Support\GovernanceFixture;
use App\Tests\Support\RunFixture;
use PHPUnit\Framework\Attributes\DataProvider;
use Symfony\Bundle\FrameworkBundle\Console\Application;
use Symfony\Bundle\FrameworkBundle\Test\KernelTestCase;
use Symfony\Component\Console\Tester\CommandTester;

final class Tv4ReportGovernanceCommandTest extends KernelTestCase
{
    private ?RunFixture $copy = null;

    protected function tearDown(): void
    {
        $this->copy?->cleanup();
        parent::tearDown();
    }

    private function render(RunFixture $run): string
    {
        $tester = new CommandTester((new Application(self::bootKernel()))->find('tv4:report'));
        $output = $run->path('rapport.html');
        self::assertSame(0, $tester->execute(['run_dir' => $run->directory, '--output' => $output]), $tester->getDisplay());
        $html = file_get_contents($output);
        self::assertIsString($html);

        return $html;
    }

    public function testAnOldRunExplicitlyReportsItsMissingContext(): void
    {
        $run = $this->copy = RunFixture::copy();
        $html = $this->render($run);
        self::assertStringContainsString('Essais et lectures', $html);
        self::assertStringContainsString('Ce run ne porte pas de contexte d’essais et de lectures.', $html);
        self::assertStringNotContainsString('N déclaré</th><td>0', $html);
    }

    /** @return iterable<string, array{string}> */
    public static function reportShapes(): iterable
    {
        yield 'simple' => ['simple'];
        yield 'profils' => ['profiles'];
        yield 'lignes' => ['lines'];
        yield 'lignes de profils' => ['lines_profiles'];
    }

    #[DataProvider('reportShapes')]
    public function testTheTripletIsRenderedDeterministicallyInEveryReport(string $shape): void
    {
        $run = $this->copy = match ($shape) {
            'profiles' => RunFixture::copyProfiles(),
            'lines' => RunFixture::copyLines(),
            'lines_profiles' => RunFixture::copyLines(RunFixture::LINES_PROFILES_SOURCE),
            default => RunFixture::copy(),
        };
        $file = match ($shape) {
            'profiles' => 'baseline/manifest.json',
            'lines' => 'full/manifest.json',
            'lines_profiles' => 'full/baseline/manifest.json',
            default => 'manifest.json',
        };
        $block = match ($shape) {
            'lines' => 'DEV',
            'lines_profiles' => 'HOLDOUT',
            default => null,
        };
        $run->setJson($file, ['governance'], GovernanceFixture::context($run, $file, $block));
        if ('lines_profiles' === $shape) {
            $run->setJson('full/baseline/metrics.json', ['research', 'guard'], 'guarded');
        }
        $html = $this->render($run);
        self::assertSame($html, $this->render($run));
        self::assertSame(1, substr_count($html, 'id="essais-lectures"'));
        self::assertStringContainsString('N déclaré</th><td>50', $html);
        self::assertStringContainsString('N_eff</th><td>Non calculé', $html);
        self::assertStringContainsString('Sharpe de bruit q95</th><td>1,38', $html);
        self::assertStringContainsString('Espérance BLP</th><td>1,02', $html);
        self::assertStringContainsString('Approximation q90</th><td>1,25', $html);
        self::assertStringContainsString('Durée de sélection T</th><td>5,00 ans', $html);
        self::assertStringContainsString('frozen/H-001', $html);
        self::assertStringContainsString('2026-01-02T03:04:05Z', $html);
        self::assertStringContainsString('Essai inscrit</th><td>17', $html);
        self::assertStringContainsString('Lecture inscrite</th><td>23', $html);
        self::assertStringNotContainsString($run->directory, $html);
        self::assertStringNotContainsString('(S13)', $html);
        if ('lines_profiles' === $shape) {
            self::assertStringContainsString('Lecture protégée avec garde-fou.', $html);
            self::assertStringContainsString('Claim HOLDOUT</th><td>3', $html);
            // Les autres profils historiques du dossier restent avertis dans l'en-tête des lignes.
            $fiche = substr($html, (int) strpos($html, '<article class="run">'));
            self::assertStringNotContainsString('Lecture protégée sans garde-fou.', $fiche);
        }
    }

    /** @return iterable<string, array{bool}> */
    public static function historicalGroups(): iterable
    {
        yield 'profils' => [false];
        yield 'lignes de profils' => [true];
    }

    #[DataProvider('historicalGroups')]
    public function testNonBaselineHistoricalReadsKeepTheirWarning(bool $lines): void
    {
        $run = $this->copy = RunFixture::copyProfiles($lines
            ? RunFixture::LINES_PROFILES_SOURCE
            : RunFixture::LINES_PROFILES_SOURCE . '/full');
        foreach ($lines ? ['full/', 'okx_usdc/'] : [''] as $prefix) {
            $file = $prefix . 'baseline/manifest.json';
            $run->setJson($file, ['governance'], GovernanceFixture::context($run, $file, 'HOLDOUT'));
            $run->setJson($prefix . 'baseline/metrics.json', ['research', 'guard'], 'guarded');
        }
        $html = $this->render($run);
        self::assertStringContainsString('Lecture protégée sans garde-fou.', $html);
        self::assertStringContainsString('Lecture protégée avec garde-fou.', $html);
    }

    public function testHistoricalLinesKeepTheirWarningWithoutClaimingGuardsAreUnavailable(): void
    {
        $run = $this->copy = RunFixture::copyLines(RunFixture::LINES_GUARD_SOURCE);
        $html = $this->render($run);
        self::assertStringContainsString('Lecture protégée sans garde-fou.', $html);
        self::assertStringNotContainsString('ne sont pas livrés', $html);
    }

    public function testHistoricalUnguardedReadsKeepTheirWarning(): void
    {
        $run = $this->copy = RunFixture::copy(RunFixture::LINES_PROFILES_SOURCE . '/full/baseline');
        $html = $this->render($run);
        self::assertStringContainsString('Lecture protégée sans garde-fou.', $html);
        self::assertStringContainsString('Ce run ne porte pas de contexte d’essais et de lectures.', $html);
        self::assertStringNotContainsString('ne sont pas livrés', $html);
    }
}
