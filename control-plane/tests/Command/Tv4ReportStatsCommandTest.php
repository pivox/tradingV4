<?php

declare(strict_types=1);

namespace App\Tests\Command;

use App\Tests\Support\RunFixture;
use App\Run\Stats\StatsValidator;
use PHPUnit\Framework\Attributes\DataProvider;
use Symfony\Bundle\FrameworkBundle\Console\Application;
use Symfony\Bundle\FrameworkBundle\Test\KernelTestCase;
use Symfony\Component\Console\Tester\CommandTester;

final class Tv4ReportStatsCommandTest extends KernelTestCase
{
    /** @return iterable<string, array{string}> */
    public static function layouts(): iterable
    {
        foreach (['simple', 'profiles', 'lines', 'lines-profiles'] as $layout) {
            yield $layout => [$layout];
        }
    }

    private function tester(): CommandTester
    {
        return new CommandTester((new Application(self::bootKernel()))->find('tv4:report'));
    }

    #[DataProvider('layouts')]
    public function testRealRustLayoutsRenderEachStatsSectionOnce(string $layout): void
    {
        $copy = RunFixture::copyProfiles(__DIR__ . '/../fixtures/stats-rust/' . $layout);
        try {
            $tester = $this->tester();
            $code = $tester->execute(['run_dir' => $copy->directory, '--output' => $copy->path('rapport.html')]);
            self::assertSame(0, $code, $tester->getDisplay());
            $html = $copy->read('rapport.html');
            foreach (['bootstrap', 'monte-carlo', 'regimes', 'concentration', 'benchmarks', 'clustering'] as $id) {
                self::assertSame(1, substr_count($html, 'id="stats-' . $id . '-titre"'), $id);
            }
            self::assertStringContainsString('id="distribution-titre"', $html);
            self::assertStringContainsString('LB90', $html);
            self::assertStringContainsString('DD / capital initial', $html);
            self::assertStringContainsString('DD / sommet', $html);
            self::assertStringContainsString('36 mois calendaires', $html);
            self::assertStringContainsString('N_eff global', $html);
            self::assertStringContainsString('panel', $html);
            self::assertStringContainsString('5 ans', $html);
            self::assertStringContainsString('Périmètre des statistiques', $html);
            if ('lines-profiles' === $layout) {
                self::assertStringContainsString('Rejeu avec retard réel', $html);
                self::assertStringContainsString('240 min', $html);
                self::assertStringContainsString('Éligibilité DEV à G2 : non', $html);
            }
            $tester->execute(['run_dir' => $copy->directory, '--output' => $copy->path('rapport2.html')]);
            self::assertSame($html, $copy->read('rapport2.html'));
        } finally {
            $copy->cleanup();
        }
    }

    public function testMalformedSidecarFailsExplicitlyWithoutPublishing(): void
    {
        $copy = RunFixture::copy();
        try {
            $copy->write('stats.json', '{broken');
            $tester = $this->tester();
            self::assertSame(1, $tester->execute(['run_dir' => $copy->directory, '--output' => $copy->path('rapport.html')]));
            self::assertStringContainsString('stats.json', $tester->getDisplay());
            self::assertFileDoesNotExist($copy->path('rapport.html'));
        } finally {
            $copy->cleanup();
        }
    }

    public function testStatsReasonsAreEscaped(): void
    {
        $copy = RunFixture::copyProfiles(__DIR__ . '/../fixtures/stats-rust/simple');
        try {
            $copy->replace('stats.json', 'aucun mois au PnL strictement positif', '<img src=x onerror=alert(1)>');
            $tester = $this->tester();
            self::assertSame(0, $tester->execute(['run_dir' => $copy->directory, '--output' => $copy->path('rapport.html')]));
            self::assertStringContainsString('&lt;img src=x onerror=alert(1)&gt;', $copy->read('rapport.html'));
            self::assertStringNotContainsString('<img', $copy->read('rapport.html'));
        } finally {
            $copy->cleanup();
        }
    }

    public function testObservedCoverageAndCompleteDailyPanelHaveDistinctLabels(): void
    {
        $copy = RunFixture::copyProfiles(__DIR__ . '/../fixtures/stats-rust/simple');
        try {
            $stats = StatsValidator::decode($copy->read('stats.json'));
            $copy->setJson('stats.json', ['corpus', 'members', '0', 'daily'], $stats->data['benchmarks']['btc']['daily']);
            $copy->setJson('stats.json', ['corpus', 'complete_daily_configs'], 0);
            $copy->setJson('stats.json', ['corpus', 'clustering'], [
                'availability' => 'unavailable', 'value' => null, 'reasons' => ['aucune série complète'],
            ]);
            $tester = $this->tester();
            self::assertSame(0, $tester->execute(['run_dir' => $copy->directory, '--output' => $copy->path('rapport.html')]));
            self::assertStringContainsString('Configurations observées (M) : 1 ; séries quotidiennes complètes : 0', $copy->read('rapport.html'));
            self::assertStringContainsString('couverture M/N : 20,00', $copy->read('rapport.html'));
        } finally {
            $copy->cleanup();
        }
    }
}
