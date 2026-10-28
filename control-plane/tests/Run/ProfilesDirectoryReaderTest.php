<?php

declare(strict_types=1);

namespace App\Tests\Run;

use App\Run\Csv\EquityCsvReader;
use App\Run\Csv\TradesCsvReader;
use App\Run\InvalidRunException;
use App\Run\ProfilesDirectoryReader;
use App\Run\RunDirectoryReader;
use App\Tests\Support\RunFixture;
use PHPUnit\Framework\Attributes\DataProvider;
use PHPUnit\Framework\TestCase;

final class ProfilesDirectoryReaderTest extends TestCase
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
        return $this->copy = RunFixture::copyProfiles();
    }

    public function testReadsTheFixture(): void
    {
        $report = $this->reader()->read(RunFixture::PROFILES_SOURCE);

        self::assertSame(str_repeat('9f0f1e50', 8), $report->profilesId);
        self::assertSame('blake3:' . str_repeat('da7a9001', 8), $report->datasetHash);
        self::assertSame(300.0, $report->initialCash);
        self::assertSame(1, $report->seed);
        self::assertSame('okx_regular', $report->feeGrid);
        self::assertNull($report->costMeasuresHash);
        self::assertFalse($report->stressCalibrated);
        self::assertSame('fill_dependent', $report->label);
        self::assertSame('dépend du fill', $report->labelFr);

        self::assertSame(['baseline', 'adverse', 'maker'], array_map(static fn ($p): string => $p->name, $report->profiles));
        self::assertTrue($report->profiles[0]->isGate());
        self::assertFalse($report->profiles[2]->isGate());
        self::assertFalse($report->profiles[0]->positive);
        self::assertTrue($report->profiles[2]->positive);
        self::assertSame(10, $report->profiles[1]->nTrades);
        self::assertNotNull($report->profiles[0]->costRMean);

        self::assertSame('positive', $report->maker->status);
        self::assertSame(10, $report->maker->entriesConverted);
        self::assertSame(3, $report->maker->entriesTakenAtArrival);
        self::assertSame(7, $report->maker->entriesFilledMaker);
        self::assertSame(0.7, $report->maker->makerEntryMakerShare);
        self::assertSame(0.5, $report->maker->minMakerEntryShare);

        self::assertFalse($report->gates->bothGateProfilesPositive);
        self::assertFalse($report->gates->costThirdBaseline);
        self::assertFalse($report->gates->costRAdverse);
        self::assertFalse($report->gates->grossMedianVs2xBaselineCost);

        self::assertSame(['baseline', 'adverse', 'maker'], array_keys($report->runs));
        self::assertSame('baseline', $report->baseline()->metrics->costs?->profile);
        self::assertSame('maker', $report->run('maker')->metrics->costs?->profile);
        self::assertSame(0.10, $report->costRLimit('adverse'));
    }

    public function testIsProfilesDirectory(): void
    {
        self::assertTrue(ProfilesDirectoryReader::isProfilesDirectory(RunFixture::PROFILES_SOURCE));
        self::assertTrue(ProfilesDirectoryReader::isProfilesDirectory(RunFixture::PROFILES_SOURCE . '/'));
        self::assertFalse(ProfilesDirectoryReader::isProfilesDirectory(RunFixture::PROFILES_SOURCE . '/baseline'));
        self::assertFalse(ProfilesDirectoryReader::isProfilesDirectory(RunFixture::SOURCE));
    }

    public function testMissingDirectory(): void
    {
        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('dossier de run introuvable');
        $this->reader()->read(sys_get_temp_dir() . '/tv4-n-existe-pas');
    }

    public function testMissingProfilesFile(): void
    {
        $copy = $this->copy();
        $copy->remove('profiles.json');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('profiles.json : fichier absent');
        $this->reader()->read($copy->directory);
    }

    public function testInvalidJson(): void
    {
        $copy = $this->copy();
        $copy->write('profiles.json', '{"schema": ');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('profiles.json : JSON invalide');
        $this->reader()->read($copy->directory);
    }

    public function testMissingSchemaKey(): void
    {
        $copy = $this->copy();
        $copy->write('profiles.json', '{"label": "negative"}');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('profiles.json : clé absente à schema');
        $this->reader()->read($copy->directory);
    }

    public function testUnknownSchema(): void
    {
        $copy = $this->copy();
        $copy->replace('profiles.json', '"tv4-profiles-v1"', '"tv4-profiles-v2"');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('profiles.json : schéma inconnu : "tv4-profiles-v2" (attendu "tv4-profiles-v1")');
        $this->reader()->read($copy->directory);
    }

    /**
     * @return iterable<string, array{string}>
     */
    public static function subDirectories(): iterable
    {
        yield 'baseline' => ['baseline'];
        yield 'adverse' => ['adverse'];
        yield 'maker' => ['maker'];
    }

    #[DataProvider('subDirectories')]
    public function testAnIncompleteProfilesDirectoryNamesTheMissingFolder(string $name): void
    {
        $copy = $this->copy();
        foreach (RunFixture::FILES as $file) {
            $copy->remove($name . '/' . $file);
        }
        rmdir($copy->path($name));

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('dossier de run introuvable : ' . $copy->path($name));
        $this->reader()->read($copy->directory);
    }

    public function testAMissingManifestInASubDirectoryNamesTheFile(): void
    {
        $copy = $this->copy();
        $copy->remove('adverse/manifest.json');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('manifest.json : fichier absent');
        $this->reader()->read($copy->directory);
    }

    public function testTwoProfilesRequiredNotFewer(): void
    {
        $copy = $this->copy();
        $copy->setJson('profiles.json', ['profiles'], []);

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('profiles.json : profiles : trois profils attendus (baseline, adverse, maker), trouvé : aucun');
        $this->reader()->read($copy->directory);
    }

    public function testAProfileCannotAppearTwice(): void
    {
        $copy = $this->copy();
        $copy->replace('profiles.json', '"name": "maker"', '"name": "adverse"');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('trois profils attendus (baseline, adverse, maker), trouvé : baseline, adverse, adverse');
        $this->reader()->read($copy->directory);
    }

    public function testUnknownProfileName(): void
    {
        $copy = $this->copy();
        $copy->replace('profiles.json', '"name": "maker"', '"name": "optimiste"');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('profiles.json : profiles[2].name : valeur inconnue "optimiste" (attendu : baseline, adverse, maker)');
        $this->reader()->read($copy->directory);
    }

    public function testASubDirectoryNameCannotEscapeTheProfilesDirectory(): void
    {
        $copy = $this->copy();
        $copy->replace('profiles.json', '"dir": "baseline"', '"dir": "../baseline"');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('profiles.json : profiles[0].dir : nom de sous-dossier simple attendu, trouvé "../baseline"');
        $this->reader()->read($copy->directory);
    }

    public function testUnknownLabel(): void
    {
        $copy = $this->copy();
        $copy->setJson('profiles.json', ['label'], 'excellent');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('profiles.json : label : valeur inconnue "excellent"');
        $this->reader()->read($copy->directory);
    }

    public function testUnknownMakerStatus(): void
    {
        $copy = $this->copy();
        $copy->setJson('profiles.json', ['maker', 'status'], 'peut_etre');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('profiles.json : status : valeur inconnue "peut_etre"');
        $this->reader()->read($copy->directory);
    }

    public function testUnknownRole(): void
    {
        $copy = $this->copy();
        $copy->replace('profiles.json', '"role": "informative"', '"role": "decor"');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('profiles.json : profiles[2].role : valeur inconnue "decor"');
        $this->reader()->read($copy->directory);
    }

    public function testAMissingGateKeyNamesItsPath(): void
    {
        $copy = $this->copy();
        $copy->unsetJson('profiles.json', ['gates', 'cost_third']);

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('profiles.json : clé absente à gates.cost_third');
        $this->reader()->read($copy->directory);
    }

    public function testNullableValuesAreAccepted(): void
    {
        $copy = $this->copy();
        $copy->setJson('profiles.json', ['maker', 'maker_entry_maker_share'], null);
        $copy->setJson('profiles.json', ['maker', 'expectancy_net'], null);
        $copy->setJson('profiles.json', ['gates', 'cost_r', 'baseline'], null);
        $copy->setJson('profiles.json', ['gates', 'gross_median_vs_2x_baseline_cost'], null);

        $report = $this->reader()->read($copy->directory);

        self::assertNull($report->maker->makerEntryMakerShare);
        self::assertNull($report->maker->expectancyNet);
        self::assertNull($report->gates->costRBaseline);
        self::assertNull($report->gates->grossMedianVs2xBaselineCost);
    }

    public function testASubRunThatIsNotS10IsRefused(): void
    {
        $copy = $this->copy();
        foreach (RunFixture::FILES as $file) {
            copy(RunFixture::PORTFOLIO_SOURCE . '/' . $file, $copy->path('baseline/' . $file));
        }

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('profiles.json : profiles[0].dir : "baseline" n\'est pas un run s10');
        $this->reader()->read($copy->directory);
    }

    /**
     * Ce que `profiles.json` répète d'un run doit être ce que le run a écrit : chaque écart est refusé.
     *
     * @return iterable<string, array{list<string>, mixed, string}>
     */
    public static function inconsistencies(): iterable
    {
        yield 'run_id' => [['profiles', '0', 'run_id'], str_repeat('0', 64), 'profiles[0].run_id : différent de celui du manifeste de baseline'];
        yield 'config_hash' => [['profiles', '1', 'config_hash'], str_repeat('0', 64), 'profiles[1].config_hash : différent de celui du manifeste de adverse'];
        yield 'result_hash' => [['profiles', '2', 'result_hash'], 'blake3:' . str_repeat('0', 64), 'profiles[2].result_hash : différent de celui du manifeste de maker'];
        yield 'verdict' => [['profiles', '0', 'verdict'], 'ok', 'profiles[0].verdict : différent de celui des métriques de baseline'];
        yield 'n_trades' => [['profiles', '1', 'n_trades'], 11, 'profiles[1].n_trades : différent de celui des métriques de adverse'];
        yield 'dataset_hash' => [['dataset_hash'], 'blake3:' . str_repeat('0', 64), 'profiles.json : dataset_hash : différent de celui du run baseline'];
        yield 'costs_file_hash' => [['costs_file_hash'], 'blake3:' . str_repeat('0', 64), 'profiles.json : costs_file_hash : différent de celui du run baseline'];
        yield 'fee_grid' => [['fee_grid'], 'bitvavo_usdc', 'profiles.json : fee_grid : différent de celui du run baseline'];
        yield 'seed' => [['seed'], 2, 'profiles.json : seed : différent de celui du run baseline'];
        yield 'stress_calibrated' => [['stress_calibrated'], true, 'profiles.json : stress_calibrated : différent de celui du run baseline'];
        yield 'profiles_id' => [['profiles_id'], str_repeat('0', 64), 'profiles.json : profiles_id : différent de celui du manifeste de baseline'];
    }

    /**
     * @param list<string> $path
     */
    #[DataProvider('inconsistencies')]
    public function testInconsistenciesWithTheRunsAreRefused(array $path, mixed $value, string $message): void
    {
        $copy = $this->copy();
        $copy->setJson('profiles.json', $path, $value);

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage($message);
        $this->reader()->read($copy->directory);
    }

    public function testAProfileThatIsNotTheRunProfileIsRefused(): void
    {
        $copy = $this->copy();
        // `profiles.json` désigne maker/ pour le profil adverse : le run de maker/ porte un autre profil.
        $copy->replace('profiles.json', '"dir": "adverse"', '"dir": "maker"');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('profiles.json : profiles[1].name : le run porte le profil "maker", pas "adverse"');
        $this->reader()->read($copy->directory);
    }
}
