<?php

declare(strict_types=1);

namespace App\Tests\Run;

use App\Run\Csv\EquityCsvReader;
use App\Run\Csv\TradesCsvReader;
use App\Run\InvalidRunException;
use App\Run\RunDirectoryReader;
use App\Tests\Support\GovernanceFixture;
use App\Tests\Support\RunFixture;
use PHPUnit\Framework\Attributes\DataProvider;
use PHPUnit\Framework\TestCase;

final class RunDirectoryReaderGovernanceTest extends TestCase
{
    private ?RunFixture $copy = null;

    protected function tearDown(): void
    {
        $this->copy?->cleanup();
    }

    private function reader(): RunDirectoryReader
    {
        return new RunDirectoryReader(new TradesCsvReader(), new EquityCsvReader());
    }

    private function copy(): RunFixture
    {
        $run = $this->copy = RunFixture::copy();
        $run->setJson('manifest.json', ['governance'], GovernanceFixture::context($run));

        return $run;
    }

    public function testReadsAnOptionalTypedContextWithoutRecalculatingNoise(): void
    {
        $run = $this->reader()->read($this->copy()->directory);
        $governance = $run->manifest->governance;
        self::assertNotNull($governance);
        self::assertSame(17, $governance->trialId);
        self::assertSame(23, $governance->readId);
        self::assertSame(50, $governance->noise->n);
        self::assertNull($governance->noise->nEff);
        self::assertSame(1.37867, $governance->noise->q95);
        self::assertSame('H-001', $governance->hypothesis?->identifiant);
    }

    public function testAcceptsUtcNanosecondsAndNullableNoiseWithoutInventingEstimates(): void
    {
        $run = $this->copy();
        $run->setJson('manifest.json', ['governance', 'frozen_at'], '2026-01-02T03:04:05.123456789+00:00');
        $run->setJson('manifest.json', ['governance', 'noise', 'n'], 1);
        $run->setJson('manifest.json', ['governance', 'noise', 'expected'], 0);
        $run->setJson('manifest.json', ['governance', 'noise', 'approx_q90'], 0);
        $run->setJson('manifest.json', ['governance', 'noise', 'q95'], null);
        $noise = $this->reader()->read($run->directory)->manifest->governance?->noise;
        self::assertNotNull($noise);
        self::assertSame(0.0, $noise->expected);
        self::assertSame(0.0, $noise->approxQ90);
        self::assertNull($noise->q95);
    }

    public function testOldRunsWithoutContextRemainReadable(): void
    {
        self::assertNull($this->reader()->read(RunFixture::SOURCE)->manifest->governance);
        $run = $this->copy();
        $run->setJson('manifest.json', ['governance'], null);
        self::assertNull($this->reader()->read($run->directory)->manifest->governance);
    }

    /** @return iterable<string, array{list<string>, mixed}> */
    public static function invalidValues(): iterable
    {
        yield 'schema' => [['schema'], 'tv4-governance-v2'];
        yield 'config_hash' => [['config_hash'], 'wrong'];
        yield 'dataset_hash' => [['dataset_hash'], 'wrong'];
        yield 'trial_id nul' => [['trial_id'], 0];
        yield 'read_id type' => [['read_id'], '23'];
        yield 'read_id négatif' => [['read_id'], -1];
        yield 'generation' => [['generation'], -1];
        yield 'n' => [['noise', 'n'], 0];
        yield 'n_eff supérieur à N' => [['noise', 'n_eff'], 51];
        yield 'modèle interdit' => [['hypothesis', 'identifiant'], 'H-000'];
        yield 'identifiant malformé' => [['hypothesis', 'identifiant'], '../../bad'];
        yield 'n_eff' => [['noise', 'n_eff'], 0];
        yield 'T nul' => [['noise', 'selection_years'], 0];
        yield 'T négatif' => [['noise', 'selection_years'], -1];
        yield 'T infini' => [['noise', 'selection_years'], 'INF'];
        yield 'q95' => [['noise', 'q95'], '1.37'];
        yield 'expected' => [['noise', 'expected'], false];
        yield 'SHA' => [['hypothesis', 'sha256'], 'bad'];
        yield 'bloc inconnu' => [['source_block'], 'OTHER'];
        yield 'bloc effectif inconnu' => [['effective_block'], 'OTHER'];
        yield 'source seule' => [['source_block'], 'DEV'];
        yield 'effectif seul' => [['effective_block'], 'DEV'];
        yield 'claim superflu' => [['claim_holdout'], 1];
        yield 'gel invalide' => [['frozen_at'], 'hier'];
        yield 'gel heure impossible' => [['frozen_at'], '2026-01-02T99:04:05Z'];
        yield 'gel date impossible' => [['frozen_at'], '2026-02-31T03:04:05Z'];
        yield 'gel hors UTC' => [['frozen_at'], '2026-01-02T03:04:05+01:00'];
    }

    /** @param list<string> $path */
    #[DataProvider('invalidValues')]
    public function testRejectsInvalidValuesWithFileAndJsonPath(array $path, mixed $value): void
    {
        $run = $this->copy();
        $run->setJson('manifest.json', ['governance', ...$path], $value);
        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('manifest.json : governance.');
        $this->reader()->read($run->directory);
    }

    public function testRequiresEveryFieldOfAPresentContext(): void
    {
        $run = $this->copy();
        $run->unsetJson('manifest.json', ['governance', 'noise', 'n_eff']);
        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('manifest.json : clé absente à governance.noise.n_eff');
        $this->reader()->read($run->directory);
    }

    /** @return iterable<string, array{string, string, int}> */
    public static function blockFrames(): iterable
    {
        yield 'DEV initial' => ['DEV', 'DEV', 0];
        yield 'VALIDATION initial' => ['VALIDATION', 'VALIDATION', 0];
        yield 'HOLDOUT initial' => ['HOLDOUT', 'HOLDOUT', 0];
        yield 'rotation HOLDOUT' => ['HOLDOUT', 'VALIDATION', 1];
        yield 'rotation FORWARD' => ['FORWARD', 'HOLDOUT', 2];
        yield 'DEV après rotation' => ['DEV', 'DEV', 1];
    }

    #[DataProvider('blockFrames')]
    public function testReadsGuardedResearchAndRotatedFrames(string $source, string $effective, int $generation): void
    {
        $run = $this->copy = RunFixture::copy(RunFixture::RESEARCH_SOURCE);
        $context = GovernanceFixture::context($run, block: $effective);
        $context['source_block'] = $source;
        $context['generation'] = $generation;
        $run->setJson('manifest.json', ['governance'], $context);
        $run->setJson('manifest.json', ['data', 'block'], $effective);
        $run->setJson('metrics.json', ['research', 'block'], $effective);
        $run->setJson('metrics.json', ['research', 'identity', 'block'], $effective);
        $run->setJson('metrics.json', ['research', 'guard'], in_array($effective, ['VALIDATION', 'HOLDOUT'], true) ? 'guarded' : 'none');
        $report = $this->reader()->read($run->directory);
        self::assertSame($effective, $report->manifest->governance?->effectiveBlock);
    }

    /** @return iterable<string, array{string, mixed}> */
    public static function invalidProtection(): iterable
    {
        yield 'fiche' => ['hypothesis', null];
        yield 'tag' => ['frozen_tag', null];
        yield 'tag incohérent' => ['frozen_tag', 'frozen/H-002'];
        yield 'date' => ['frozen_at', null];
        yield 'claim absent' => ['claim_holdout', null];
        yield 'claim nul' => ['claim_holdout', 0];
        yield 'trame sans rotation' => ['source_block', 'FORWARD'];
        yield 'effectif incohérent' => ['effective_block', 'VALIDATION'];
    }

    #[DataProvider('invalidProtection')]
    public function testRejectsIncompleteProtection(string $key, mixed $value): void
    {
        $run = $this->copy = RunFixture::copy(RunFixture::LINES_PROFILES_SOURCE . '/full/baseline');
        $run->setJson('manifest.json', ['governance'], GovernanceFixture::context($run, block: 'HOLDOUT'));
        $run->setJson('metrics.json', ['research', 'guard'], 'guarded');
        $run->setJson('manifest.json', ['governance', $key], $value);
        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('manifest.json : governance.');
        $this->reader()->read($run->directory);
    }

    public function testContextDoesNotTurnAnUnguardedReadIntoAGuardedRead(): void
    {
        $run = $this->copy = RunFixture::copy(RunFixture::LINES_PROFILES_SOURCE . '/full/baseline');
        $run->setJson('manifest.json', ['governance'], GovernanceFixture::context($run, block: 'HOLDOUT'));
        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json : research.guard');
        $this->reader()->read($run->directory);
    }

    public function testGuardedRequiresItsProvenance(): void
    {
        $run = $this->copy = RunFixture::copy(RunFixture::RESEARCH_SOURCE);
        $run->setJson('metrics.json', ['research', 'guard'], 'guarded');
        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('manifest.json : governance');
        $this->reader()->read($run->directory);
    }
}
