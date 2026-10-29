<?php

declare(strict_types=1);

namespace App\Tests\Run;

use App\Run\Csv\EquityCsvReader;
use App\Run\Csv\TradesCsvReader;
use App\Run\InvalidRunException;
use App\Run\ProfilesDirectoryReader;
use App\Run\RunDirectoryReader;
use App\Tests\Support\RunFixture;
use PHPUnit\Framework\TestCase;

/**
 * Dossier de profils d'un run de recherche (docs/specs/s11.md §9.6) : trois runs s11 joués avec les profils de coûts,
 * lus par le lecteur de profils de S10. Les trois runs doivent être de la même génération, du même bloc, de la même
 * ligne et de la même fenêtre de décision.
 */
final class ProfilesDirectoryReaderS11Test extends TestCase
{
    private ?RunFixture $copy = null;

    protected function tearDown(): void
    {
        $this->copy?->cleanup();
        $this->copy = null;
    }

    private function reader(): ProfilesDirectoryReader
    {
        return new ProfilesDirectoryReader(new RunDirectoryReader(new TradesCsvReader(), new EquityCsvReader()));
    }

    private function copy(): RunFixture
    {
        return $this->copy = RunFixture::copyProfiles(RunFixture::LINES_PROFILES_SOURCE . '/okx_usdc');
    }

    public function testReadsAProfilesDirectoryOfS11Runs(): void
    {
        $report = $this->reader()->read(RunFixture::LINES_PROFILES_SOURCE . '/full');

        self::assertSame(['baseline', 'adverse', 'maker'], array_keys($report->runs));
        foreach ($report->runs as $name => $run) {
            self::assertSame('tv4-metrics-s11', $run->metrics->schema, $name);
            self::assertTrue($run->metrics->isCosts(), $name);
            self::assertSame($name, $run->metrics->costs?->profile);
            self::assertSame('full', $run->metrics->research?->line, $name);
            self::assertSame($run->metrics->nTrades, $report->profile($name)->nTrades, $name);
        }
        self::assertSame(
            $report->run('baseline')->metrics->research?->decisionWindow->startMs,
            $report->run('maker')->metrics->research?->decisionWindow->startMs,
        );
    }

    public function testTheThreeProfilesMustBeOfTheSameGeneration(): void
    {
        $run = $this->copy();
        $run->replace('adverse/manifest.json', '"tv4-manifest-s11"', '"tv4-manifest-s10"');
        $run->replace('adverse/metrics.json', '"tv4-metrics-s11"', '"tv4-metrics-s10"');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('profiles.json : profiles[1].dir : "adverse" n\'est pas un run de la même génération que "baseline" (tv4-metrics-s10 contre tv4-metrics-s11)');

        $this->reader()->read($run->directory);
    }

    public function testTheThreeProfilesMustShareTheBlockTheLineAndTheDecisionWindow(): void
    {
        $run = $this->copy();
        $run->setJson('maker/metrics.json', ['research', 'block'], 'DEV');
        $run->setJson('maker/metrics.json', ['research', 'identity', 'block'], 'DEV');
        $run->setJson('maker/manifest.json', ['data', 'block'], 'DEV');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('profiles.json : profiles[2].dir : "maker" n\'a pas le bloc, la ligne et la fenêtre de décision de "baseline"');

        $this->reader()->read($run->directory);
    }

    public function testAnS11ProfileWithoutCostBlocksIsRejected(): void
    {
        $run = $this->copy();
        $single = RunFixture::copy(RunFixture::RESEARCH_SOURCE);
        try {
            foreach (RunFixture::FILES as $file) {
                $run->write('baseline/' . $file, $single->read($file));
            }
            $this->expectException(InvalidRunException::class);
            $this->expectExceptionMessage('profiles[0].dir : "baseline" est un run s11 sans profil de coûts (bloc costs absent)');

            $this->reader()->read($run->directory);
        } finally {
            $single->cleanup();
        }
    }
}
