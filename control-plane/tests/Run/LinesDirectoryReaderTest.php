<?php

declare(strict_types=1);

namespace App\Tests\Run;

use App\Run\Csv\EquityCsvReader;
use App\Run\Csv\TradesCsvReader;
use App\Run\Dto\LinesReport;
use App\Run\InvalidRunException;
use App\Run\LinesDirectoryReader;
use App\Run\ProfilesDirectoryReader;
use App\Run\RunDirectoryReader;
use App\Tests\Support\RunFixture;
use PHPUnit\Framework\Attributes\DataProvider;
use PHPUnit\Framework\TestCase;

/**
 * Lecture d'un dossier de lignes (`lines.json`, `tv4-lines-v1`, docs/specs/s11.md §9.3) : `run-lignes/` (sortie réelle
 * de `tv4`, deux runs), `run-lignes-garde/` (écrit à la main : deux runs aux jambes différentes, garde levée),
 * `run-lignes-profils/` (écrit à la main : deux dossiers de profils de coûts).
 */
final class LinesDirectoryReaderTest extends TestCase
{
    private ?RunFixture $copy = null;

    protected function tearDown(): void
    {
        $this->copy?->cleanup();
        $this->copy = null;
    }

    private function reader(): LinesDirectoryReader
    {
        $runs = new RunDirectoryReader(new TradesCsvReader(), new EquityCsvReader());

        return new LinesDirectoryReader($runs, new ProfilesDirectoryReader($runs));
    }

    private function copy(string $source = RunFixture::LINES_GUARD_SOURCE): RunFixture
    {
        return $this->copy = RunFixture::copyLines($source);
    }

    private function read(RunFixture $run): LinesReport
    {
        return $this->reader()->read($run->directory);
    }

    public function testDetectsALinesDirectory(): void
    {
        self::assertTrue(LinesDirectoryReader::isLinesDirectory(RunFixture::LINES_SOURCE));
        self::assertTrue(LinesDirectoryReader::isLinesDirectory(RunFixture::LINES_GUARD_SOURCE));
        self::assertTrue(LinesDirectoryReader::isLinesDirectory(RunFixture::LINES_PROFILES_SOURCE . '/'));
        self::assertFalse(LinesDirectoryReader::isLinesDirectory(RunFixture::LINES_SOURCE . '/full'));
        self::assertFalse(LinesDirectoryReader::isLinesDirectory(RunFixture::PROFILES_SOURCE));
        self::assertFalse(LinesDirectoryReader::isLinesDirectory(RunFixture::SOURCE));
    }

    public function testReadsTheRealLinesDirectory(): void
    {
        $report = $this->reader()->read(RunFixture::LINES_SOURCE);

        self::assertSame('DEV', $report->block);
        self::assertSame(['single'], $report->profileNames);
        self::assertSame(300.0, $report->initialCash);
        self::assertSame('okx_usdc_tradable', $report->subsetKind);
        self::assertSame(['BTC', 'ETH', 'SOL'], $report->subsetBases);
        self::assertNull($report->universeWindowHash);
        self::assertSame(['full', 'okx_usdc'], array_map(static fn ($l): string => $l->line, $report->lines));

        // Une seule jambe, BTC-USD, tradable : les deux lignes jouent les mêmes trades, la survivance résiduelle est nulle.
        $full = $report->full();
        $okx = $report->line('okx_usdc');
        self::assertSame(['BTC-USD'], $full->legs);
        self::assertSame(['BTC-USD'], $okx->legs);
        self::assertNotNull($full->run);
        self::assertNotNull($okx->run);
        self::assertSame($full->run->manifest->configHash, $okx->run->manifest->configHash);
        self::assertSame($report->configHash['single'], $full->run->manifest->configHash);
        self::assertNotSame($full->id, $okx->id);
        self::assertSame(4, $full->stats('single')->nTrades);
        self::assertSame(0, $full->stats('single')->tradesPurged);
        self::assertSame(0, $report->residualOf('single')->nTrades);
        self::assertEqualsWithDelta(0.0, (float) $report->residualOf('single')->returnTotal, 1e-12);
    }

    public function testReadsTwoRuns(): void
    {
        $report = $this->reader()->read(RunFixture::LINES_GUARD_SOURCE);

        self::assertSame('VALIDATION', $report->block);
        self::assertSame(['single'], $report->profileNames);
        self::assertFalse($report->isProfiles());
        self::assertSame(300.0, $report->initialCash);
        self::assertSame('okx_usdc_tradable', $report->subsetKind);
        self::assertSame(['BTC'], $report->subsetBases);
        self::assertSame(['single'], array_keys($report->configHash));
        self::assertNotNull($report->universeWindowHash);
        self::assertSame(['full', 'okx_usdc'], array_map(static fn ($l): string => $l->line, $report->lines));

        $full = $report->full();
        $okx = $report->line('okx_usdc');
        self::assertFalse($full->isProfiles());
        self::assertNull($full->profiles);
        self::assertNotNull($full->run);
        self::assertSame($full->run->manifest->runId, $full->id);
        self::assertSame(['BTC-USD', 'ETH-USD'], $full->legs);
        self::assertSame(['BTC-USD'], $okx->legs);
        self::assertSame($full->run, $full->referenceRun());
        self::assertSame('full', $full->research()->line);
        self::assertSame('okx_usdc', $okx->research()->line);

        // Les deux lignes partagent le config_hash (§9.1) et ont deux run_id.
        self::assertNotNull($okx->run);
        self::assertSame($full->run->manifest->configHash, $okx->run->manifest->configHash);
        self::assertSame($report->configHash['single'], $okx->run->manifest->configHash);
        self::assertNotSame($full->id, $okx->id);
        self::assertNotSame($full->research()->identityHash, $okx->research()->identityHash);

        $stats = $okx->stats('single');
        self::assertSame($okx->research()->identityHash, $stats->identityHash);
        self::assertSame($okx->run->metrics->nTrades, $stats->nTrades);
        self::assertSame(1, $stats->tradesPurged);
        self::assertSame($okx->run->metrics->verdict, $stats->verdict);

        $residual = $report->residualOf('single');
        self::assertSame($full->stats('single')->nTrades - $stats->nTrades, $residual->nTrades);
        self::assertNotNull($residual->returnTotal);
    }

    public function testReadsTwoDirectoriesOfProfilesInTheFixedProfileOrder(): void
    {
        $report = $this->reader()->read(RunFixture::LINES_PROFILES_SOURCE);

        self::assertTrue($report->isProfiles());
        // lines.json est écrit à clés triées (adverse, baseline, maker) : l'ordre lu est celui des profils du moteur.
        self::assertSame(['baseline', 'adverse', 'maker'], $report->profileNames);
        self::assertSame(['baseline', 'adverse', 'maker'], array_keys($report->configHash));
        self::assertSame(['baseline', 'adverse', 'maker'], array_keys($report->residual));
        foreach ($report->lines as $entry) {
            self::assertTrue($entry->isProfiles());
            self::assertNull($entry->run);
            self::assertNotNull($entry->profiles);
            self::assertSame($entry->profiles->profilesId, $entry->id);
            self::assertSame(['baseline', 'adverse', 'maker'], array_keys($entry->byProfile));
            self::assertSame($entry->profiles->baseline(), $entry->referenceRun());
        }
        $full = $report->full();
        $okx = $report->line('okx_usdc');
        self::assertNotNull($full->profiles);
        self::assertNotNull($okx->profiles);
        foreach (['baseline', 'adverse', 'maker'] as $name) {
            // Même config_hash par profil sur les deux lignes, run_id distincts.
            self::assertSame(
                $full->profiles->run($name)->manifest->configHash,
                $okx->profiles->run($name)->manifest->configHash,
            );
            self::assertNotSame($full->profiles->run($name)->manifest->runId, $okx->profiles->run($name)->manifest->runId);
            self::assertTrue($full->profiles->run($name)->metrics->isResearch());
        }
        self::assertNotSame($full->profiles->profilesId, $okx->profiles->profilesId);
        self::assertSame(['BTC-USD', 'ETH-USD'], $full->legs);
        self::assertSame(['BTC-USD'], $okx->legs);
    }

    public function testAProfilesDirectoryOfS11RunsIsReadByTheProfilesReader(): void
    {
        $runs = new RunDirectoryReader(new TradesCsvReader(), new EquityCsvReader());
        $report = (new ProfilesDirectoryReader($runs))->read(RunFixture::LINES_PROFILES_SOURCE . '/full');

        self::assertSame(['baseline', 'adverse', 'maker'], array_keys($report->runs));
        foreach ($report->runs as $run) {
            self::assertSame('tv4-metrics-s11', $run->metrics->schema);
            self::assertNotNull($run->metrics->research);
        }
    }

    public function testALinesDirectoryIsNotAProfilesDirectory(): void
    {
        self::assertFalse(ProfilesDirectoryReader::isProfilesDirectory(RunFixture::LINES_PROFILES_SOURCE));
        self::assertTrue(ProfilesDirectoryReader::isProfilesDirectory(RunFixture::LINES_PROFILES_SOURCE . '/okx_usdc'));
    }

    public function testMissingLinesFileIsRejected(): void
    {
        $run = $this->copy();
        $run->remove('lines.json');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('lines.json : fichier absent');

        $this->read($run);
    }

    public function testMissingDirectoryIsRejected(): void
    {
        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('dossier de run introuvable');

        $this->reader()->read('/chemin/inexistant');
    }

    public function testUnknownSchemaIsRejected(): void
    {
        $run = $this->copy();
        $run->setJson('lines.json', ['schema'], 'tv4-lines-v2');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('lines.json : schéma inconnu : "tv4-lines-v2" (attendu "tv4-lines-v1")');

        $this->read($run);
    }

    /**
     * @return iterable<string, array{list<string>, mixed, string}>
     */
    public static function mismatches(): iterable
    {
        yield 'bloc inconnu' => [['block'], 'BETA', 'lines.json : block : valeur inconnue "BETA"'];
        yield 'bloc différent du run' => [['block'], 'DEV', 'lines.json : block : différent de research.block du run full'];
        yield 'dataset_hash' => [['dataset_hash'], 'blake3:x', 'lines.json : dataset_hash : différent de dataset_hash du manifeste de full'];
        yield 'universe_window_hash' => [['universe_window_hash'], 'blake3:x', 'lines.json : universe_window_hash : différent de research.universe.window_hash du run full'];
        yield 'config_hash' => [['config_hash', 'single'], 'x', 'lines.json : config_hash.single : différent de config_hash du manifeste de full'];
        yield 'capital' => [['initial_cash'], 100.0, 'lines.json : initial_cash : différent de initial_cash du run full'];
        yield 'sous-ensemble : nature' => [['subset', 'kind'], 'autre', 'lines.json : subset : différent de research.subset du run okx_usdc'];
        yield 'sous-ensemble : hash' => [['subset', 'subset_hash'], 'blake3:x', 'lines.json : subset : différent de research.subset du run okx_usdc'];
        yield 'run_id' => [['lines', '0', 'run_id'], 'x', 'lines.json : lines[0].run_id : différent de celui du dossier full'];
        yield 'jambes' => [['lines', '1', 'legs'], ['BTC-USD', 'ETH-USD'], 'lines.json : lines[1].legs : différent de data.symbols du manifeste de okx_usdc'];
        yield 'identité' => [['lines', '0', 'by_profile', 'single', 'identity_hash'], 'blake3:x', 'lines[0].by_profile.single.identity_hash : différent de research.identity_hash du run full'];
        yield 'effectif' => [['lines', '1', 'by_profile', 'single', 'n_trades'], 99, 'lines[1].by_profile.single.n_trades : différent de n_trades du run okx_usdc'];
        yield 'purge' => [['lines', '0', 'by_profile', 'single', 'trades_purged'], 0, 'lines[0].by_profile.single.trades_purged : différent de research.purge.trades_purged du run full'];
        yield 'verdict' => [['lines', '0', 'by_profile', 'single', 'verdict'], 'ok', 'lines[0].by_profile.single.verdict : différent de verdict du run full'];
        yield 'sous-dossier non simple' => [['lines', '0', 'dir'], '../full', 'lines[0].dir : nom de sous-dossier simple attendu'];
        yield 'clé d\'identifiant d\'un run' => [['lines', '0', 'profiles_id'], 'x', 'lines[0].run_id : un run se désigne par run_id'];
    }

    /**
     * @param list<string> $path
     */
    #[DataProvider('mismatches')]
    public function testAMismatchBetweenLinesJsonAndTheRunsIsRejected(array $path, mixed $value, string $message): void
    {
        $run = $this->copy();
        $run->setJson('lines.json', $path, $value);

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage($message);

        $this->read($run);
    }

    public function testBothLinesMustShareTheConfigHashOfEachProfile(): void
    {
        $run = $this->copy();
        $run->setJson('okx_usdc/manifest.json', ['config_hash'], 'autre');
        $run->setJson('okx_usdc/metrics.json', ['config_hash'], 'autre');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('lines.json : config_hash.single : différent de config_hash du manifeste de okx_usdc');

        $this->read($run);
    }

    public function testRunErrorsNameTheLineDirectory(): void
    {
        $run = $this->copy();
        $run->remove('okx_usdc/trades.csv');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('okx_usdc/ : trades.csv : fichier absent');

        $this->read($run);
    }

    public function testALineThatIsNotAnS11RunIsRejected(): void
    {
        $run = $this->copy();
        $other = RunFixture::copyPortfolio();
        try {
            foreach (RunFixture::FILES as $file) {
                $run->write('okx_usdc/' . $file, $other->read($file));
            }
            $manifest = json_decode($other->read('manifest.json'), true, 512, JSON_THROW_ON_ERROR);
            self::assertIsArray($manifest);
            $run->setJson('lines.json', ['lines', '1', 'run_id'], $manifest['run_id']);
            $this->expectException(InvalidRunException::class);
            $this->expectExceptionMessage('lines[1].dir : "okx_usdc" n\'est pas un run s11');

            $this->read($run);
        } finally {
            $other->cleanup();
        }
    }

    public function testTheTwoLinesMustBeFullThenOkxUsdc(): void
    {
        $run = $this->copy();
        $lines = json_decode($run->read('lines.json'), true, 512, JSON_THROW_ON_ERROR);
        self::assertIsArray($lines);
        self::assertIsArray($lines['lines']);
        $lines['lines'] = array_reverse($lines['lines']);
        $run->write('lines.json', json_encode($lines, JSON_THROW_ON_ERROR));

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('lines.json : lines : deux lignes full, okx_usdc attendues dans cet ordre, trouvé : okx_usdc, full');

        $this->read($run);
    }

    public function testResidualMustCoverTheSameProfiles(): void
    {
        $run = $this->copy();
        $run->setJson('lines.json', ['residual_survivorship'], ['baseline' => ['n_trades' => 1]]);

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('lines.json : residual_survivorship : clés single attendues, trouvé : baseline');

        $this->read($run);
    }

    public function testASingleRunLineCannotCarryTheThreeProfileKeys(): void
    {
        $run = $this->copy();
        $run->setJson('lines.json', ['config_hash'], ['baseline' => 'a', 'adverse' => 'b', 'maker' => 'c']);

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('lines.json : config_hash : clés single attendues pour la ligne full');

        $this->read($run);
    }

    public function testProfilesLinesMustCoverTheThreeProfiles(): void
    {
        $run = $this->copy(RunFixture::LINES_PROFILES_SOURCE);
        $run->setJson('lines.json', ['config_hash'], ['single' => 'a']);

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('lines.json : config_hash : clés baseline, adverse, maker attendues pour la ligne full');

        $this->read($run);
    }

    public function testProfilesLineMismatchNamesTheProfile(): void
    {
        $run = $this->copy(RunFixture::LINES_PROFILES_SOURCE);
        $run->setJson('lines.json', ['lines', '1', 'by_profile', 'maker', 'n_trades'], 99);

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('lines[1].by_profile.maker.n_trades : différent de n_trades du run okx_usdc/maker');

        $this->read($run);
    }

    public function testAProfilesLineIsDesignatedByProfilesId(): void
    {
        $run = $this->copy(RunFixture::LINES_PROFILES_SOURCE);
        $run->setJson('lines.json', ['lines', '0', 'profiles_id'], 'x');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('lines[0].profiles_id : différent de celui du dossier full');

        $this->read($run);
    }
}
