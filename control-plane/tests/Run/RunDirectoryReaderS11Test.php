<?php

declare(strict_types=1);

namespace App\Tests\Run;

use App\Run\Csv\EquityCsvReader;
use App\Run\Csv\TradesCsvReader;
use App\Run\Dto\ResearchInfo;
use App\Run\InvalidRunException;
use App\Run\RunDirectoryReader;
use App\Tests\Support\RunFixture;
use PHPUnit\Framework\Attributes\DataProvider;
use PHPUnit\Framework\TestCase;

/**
 * Lecture d'un run s11 (`tv4-manifest-s11` et `tv4-metrics-s11`, docs/specs/s11.md §9.2) : `run-recherche/` (sortie
 * réelle de `tv4`, symboles fixes), `run-recherche-trimestriel/` (écrit à la main : univers trimestriel, exclusions
 * qualité, trade purgé), `run-lignes/` (sortie réelle, deux lignes), `run-lignes-garde/` pour la ligne `okx_usdc` et la
 * garde levée, `run-lignes-profils/` pour un run s11 joué avec un profil de coûts.
 */
final class RunDirectoryReaderS11Test extends TestCase
{
    private ?RunFixture $copy = null;

    protected function tearDown(): void
    {
        $this->copy?->cleanup();
        $this->copy = null;
    }

    private function reader(): RunDirectoryReader
    {
        return new RunDirectoryReader(new TradesCsvReader(), new EquityCsvReader());
    }

    /** Copie du run de recherche écrit à la main (univers trimestriel, exclusions qualité, trade purgé). */
    private function copy(): RunFixture
    {
        return $this->copy = RunFixture::copy(RunFixture::RESEARCH_QUARTERLY_SOURCE);
    }

    public function testReadsTheRealFixedUniverseRun(): void
    {
        $run = $this->reader()->read(RunFixture::RESEARCH_SOURCE);

        self::assertSame('tv4-manifest-s11', $run->manifest->schema);
        self::assertSame('tv4-metrics-s11', $run->metrics->schema);
        self::assertTrue($run->metrics->isResearch());
        self::assertFalse($run->metrics->isCosts());
        self::assertSame(['BTC-USD'], $run->manifest->data->symbols);
        self::assertSame('zero_volume_marked', $run->manifest->data->gapsPolicy);
        self::assertSame('DEV', $run->manifest->data->block);
        self::assertNotNull($run->manifest->data->decisionWindow);
        self::assertSame(1_514_764_800_000, $run->manifest->data->decisionWindow->startMs);
        self::assertSame(1_672_531_200_000, $run->manifest->data->decisionWindow->endMs);
        self::assertSame([], $run->manifest->researchInputs);

        $research = $run->metrics->research;
        self::assertNotNull($research);
        self::assertSame('DEV', $research->block);
        self::assertSame('none', $research->guard);
        self::assertSame('2017-06-05T00:00:00Z', $research->warmupStart);
        self::assertSame('full', $research->line);
        self::assertNull($research->subset);
        self::assertFalse($research->regimeUsed);

        // Univers fixe : pas de trimestre, pas de hash d'univers ; le TF de décision de la stratégie est écrit.
        self::assertFalse($research->universe->isQuarterly());
        self::assertSame('fixed', $research->universe->mode);
        self::assertNull($research->universe->rulesHash);
        self::assertNull($research->universe->windowHash);
        self::assertSame('15m', $research->universe->decisionTf);
        self::assertSame([], $research->universe->quarters);
        self::assertNull($research->identity->universeWindowHash);
        self::assertSame([], $research->qualityExclusions);

        // Aucun trade purgé : la fin du dataset tombe hors position.
        self::assertSame(4, $run->metrics->nTrades);
        self::assertCount(4, $run->trades);
        self::assertSame(4, $research->purge->tradesKept);
        self::assertSame(0, $research->purge->tradesPurged);
        self::assertSame(0, $research->funnel->total());
        self::assertSame(['BTC-USD'], array_map(static fn ($l): string => $l->symbol, $research->legs));
        self::assertSame('2020-01-01T12:30:00Z', $research->legs[0]->liveFrom);
    }

    public function testReadsTheQuarterlyResearchRunIntoTypedDtos(): void
    {
        $run = $this->reader()->read(RunFixture::RESEARCH_QUARTERLY_SOURCE);

        self::assertSame('tv4-manifest-s11', $run->manifest->schema);
        self::assertSame('tv4-metrics-s11', $run->metrics->schema);
        self::assertTrue($run->metrics->isResearch());
        self::assertTrue($run->metrics->isPortfolio());
        self::assertFalse($run->metrics->isCosts());
        self::assertNull($run->metrics->costs);
        self::assertSame(['BTC-USD', 'ETH-USD'], $run->manifest->data->symbols);
        self::assertSame('zero_volume_marked', $run->manifest->data->gapsPolicy);
        self::assertSame('FORWARD', $run->manifest->data->block);
        self::assertNotNull($run->manifest->data->decisionWindow);
        self::assertSame(1_782_864_000_000, $run->manifest->data->decisionWindow->startMs);
        self::assertSame(['universe' => 'research/universe/kraken_r1'], $run->manifest->researchInputs);

        // La configuration hachée de recherche vide les tables par jambe (docs/specs/s11.md §8.6).
        self::assertNotNull($run->manifest->config->research);
        self::assertSame('b1', $run->manifest->config->research['blocks_version']);
        self::assertSame('end_of_window_v1', $run->manifest->config->research['purge_rule']);
        self::assertSame([], $run->manifest->config->instruments);
        self::assertSame('[]', $run->manifest->config->portfolio['universe'] ?? null);

        // Les métriques de trades portent sur les trades conservés ; trades.csv garde tous les trades.
        self::assertSame(9, $run->metrics->nTrades);
        self::assertSame(0, $run->metrics->nTradesForced);
        self::assertCount(10, $run->trades);
        self::assertNotNull($run->metrics->executability);
        self::assertNotNull($run->metrics->funnelByPair);

        $research = $run->metrics->research;
        self::assertNotNull($research);
        self::assertSame('FORWARD', $research->block);
        self::assertSame('b1', $research->blocksVersion);
        self::assertSame('none', $research->guard);
        self::assertFalse($research->isUnguarded());
        self::assertSame(1_782_864_000_000, $research->decisionWindow->startMs);
        self::assertSame(1_783_570_200_000, $research->decisionWindow->endMs);
        self::assertSame('2026-07-01T00:00:00Z', $research->decisionWindow->start);
        self::assertSame('2025-12-03T00:00:00Z', $research->warmupStart);
        self::assertSame('full', $research->line);
        self::assertNull($research->subset);
        self::assertFalse($research->regimeUsed);
        self::assertSame('kept_trades', $research->metricsBasis);

        self::assertTrue($research->universe->isQuarterly());
        self::assertSame('15m', $research->universe->decisionTf);
        self::assertNotNull($research->universe->rulesHash);
        self::assertNotNull($research->universe->windowHash);
        self::assertCount(1, $research->universe->quarters);
        self::assertSame('2026-Q3', $research->universe->quarters[0]->quarter);
        self::assertSame(2, $research->universe->quarters[0]->members);
        self::assertSame(2, $research->universe->quarters[0]->eligibleLegs);
        self::assertSame([], $research->universe->quarters[0]->excludedQuality);

        self::assertSame($research->universe->windowHash, $research->identity->universeWindowHash);
        self::assertNull($research->identity->subsetHash);
        self::assertSame('full', $research->identity->line);
        self::assertSame(['ETH-USD'], array_keys($research->qualityExclusions));
        self::assertSame('2026-Q3', $research->qualityExclusions['ETH-USD'][0]->quarter);
        self::assertSame(['5m'], $research->qualityExclusions['ETH-USD'][0]->tfs);

        self::assertSame('end_of_window_v1', $research->purge->rule);
        self::assertSame(9, $research->purge->tradesKept);
        self::assertSame(1, $research->purge->tradesPurged);
        self::assertEqualsWithDelta(12.84993856, $research->purge->pnlNetPurged, 1e-9);

        self::assertSame(5, $research->funnel->entriesOutsideDecisionWindow);
        self::assertSame(5, $research->funnel->total());
        self::assertSame(['BTC-USD', 'ETH-USD'], array_map(static fn ($l): string => $l->symbol, $research->legs));
        self::assertSame(3, $research->legs[0]->funnel->entriesOutsideDecisionWindow);
        self::assertSame('2025-12-03T12:30:00Z', $research->legs[0]->liveFrom);
    }

    public function testReadsTheOkxLineOfTheRealLinesDirectory(): void
    {
        $run = $this->reader()->read(RunFixture::LINES_SOURCE . '/okx_usdc');

        $research = $run->metrics->research;
        self::assertNotNull($research);
        self::assertSame('DEV', $research->block);
        self::assertSame('okx_usdc', $research->line);
        self::assertFalse($research->isUnguarded());
        self::assertNotNull($research->subset);
        self::assertSame('okx_usdc_tradable', $research->subset->kind);
        self::assertSame([], $research->subset->legsExcluded);
        self::assertSame($research->subset->subsetHash, $research->identity->subsetHash);
        self::assertSame(['okx_subset' => 'smoke-s11/subset/okx_usdc.json'], $run->manifest->researchInputs);
    }

    public function testReadsTheOkxLineAndTheGuardOfAnUnguardedRun(): void
    {
        $run = $this->reader()->read(RunFixture::LINES_GUARD_SOURCE . '/okx_usdc');

        $research = $run->metrics->research;
        self::assertNotNull($research);
        self::assertSame('VALIDATION', $research->block);
        self::assertSame('okx_usdc', $research->line);
        self::assertTrue($research->isUnguarded());
        self::assertNotNull($research->subset);
        self::assertSame('okx_usdc_tradable', $research->subset->kind);
        self::assertSame(['ETH-USD'], $research->subset->legsExcluded);
        self::assertSame($research->subset->subsetHash, $research->identity->subsetHash);
        self::assertSame(['BTC-USD'], $run->manifest->data->symbols);
        self::assertEquals(
            ['universe' => 'research/universe/kraken_r1', 'okx_subset' => 'research/okx_subset/okx_usdc_20261028.json'],
            $run->manifest->researchInputs,
        );
    }

    public function testAResearchRunPlayedWithACostProfileReadsTheCostBlocks(): void
    {
        $run = $this->reader()->read(RunFixture::LINES_PROFILES_SOURCE . '/full/baseline');

        self::assertSame('tv4-metrics-s11', $run->metrics->schema);
        self::assertTrue($run->metrics->isResearch());
        self::assertTrue($run->metrics->isCosts());
        self::assertNotNull($run->metrics->costs);
        self::assertSame('baseline', $run->metrics->costs->profile);
        self::assertNotNull($run->metrics->costArithmetic);
        self::assertNotNull($run->metrics->costCounters);
        self::assertSame('baseline', $run->manifest->profile);
        self::assertNull($run->manifest->config->slippageBps);
        self::assertSame('okx_regular', $run->manifest->config->feesExchange);
        self::assertNotNull($run->metrics->research);
        self::assertSame('HOLDOUT', $run->metrics->research->block);
        self::assertTrue($run->metrics->research->isUnguarded());
    }

    public function testAnS11RunWithoutCostsDoesNotRequireTheCostBlocks(): void
    {
        $run = $this->reader()->read(RunFixture::RESEARCH_SOURCE);

        self::assertNull($run->metrics->costArithmetic);
        self::assertNull($run->metrics->costCounters);
        self::assertNull($run->manifest->profile);
    }

    public function testAFixedUniverseMayOmitTheQuarterlyKeys(): void
    {
        $run = $this->copy();
        $run->setJson('metrics.json', ['research', 'universe'], ['mode' => 'fixed']);
        $run->setJson('metrics.json', ['research', 'identity', 'universe_window_hash'], null);

        $research = $this->reader()->read($run->directory)->metrics->research;

        self::assertNotNull($research);
        self::assertFalse($research->universe->isQuarterly());
        self::assertSame('fixed', $research->universe->mode);
        self::assertNull($research->universe->rulesHash);
        self::assertNull($research->universe->windowHash);
        self::assertNull($research->universe->decisionTf);
        self::assertSame([], $research->universe->quarters);
    }

    public function testAQuarterlyUniverseRequiresItsHashes(): void
    {
        $run = $this->copy();
        $run->unsetJson('metrics.json', ['research', 'universe', 'window_hash']);

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json : clé absente à research.universe.window_hash');

        $this->reader()->read($run->directory);
    }

    public function testAnS11RunWithoutAResearchBlockIsRejected(): void
    {
        $run = $this->copy();
        $run->unsetJson('metrics.json', ['research']);

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('metrics.json : clé absente à research');

        $this->reader()->read($run->directory);
    }

    public function testAnS9RunIgnoresAResearchBlock(): void
    {
        // Le bloc research n'existe qu'en s11 : un run s9 se lit comme avant, sans lire ni exiger ce bloc.
        $run = RunFixture::copyPortfolio();
        $this->copy = $run;
        $run->setJson('metrics.json', ['research'], ['block' => 'inconnu']);

        $report = $this->reader()->read($run->directory);

        self::assertNull($report->metrics->research);
        self::assertFalse($report->metrics->isResearch());
    }

    public function testTheManifestAndTheMetricsMustBeOfTheSameGeneration(): void
    {
        $run = $this->copy();
        $run->replace('metrics.json', '"tv4-metrics-s11"', '"tv4-metrics-s9"');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('"tv4-metrics-s9" ne correspond pas au schéma du manifeste "tv4-manifest-s11"');

        $this->reader()->read($run->directory);
    }

    /**
     * @return iterable<string, array{list<string>, mixed, string}>
     */
    public static function invalidResearchValues(): iterable
    {
        yield 'bloc inconnu' => [['research', 'block'], 'BETA', 'research.block : valeur inconnue "BETA" (attendu : DEV, VALIDATION, HOLDOUT, FORWARD)'];
        yield 'garde inconnue' => [['research', 'guard'], 'maybe', 'research.guard : valeur inconnue "maybe"'];
        yield 'ligne inconnue' => [['research', 'line'], 'half', 'research.line : valeur inconnue "half"'];
        yield 'mode d\'univers inconnu' => [['research', 'universe', 'mode'], 'weekly', 'research.universe.mode : valeur inconnue "weekly"'];
        yield 'fenêtre inversée' => [['research', 'decision_window', 'end_ms'], 1, 'research.decision_window : le début doit précéder la fin'];
        yield 'identité : bloc' => [['research', 'identity', 'block'], 'DEV', 'research.identity.block : différent de celui du bloc research'];
        yield 'identité : ligne' => [['research', 'identity', 'line'], 'okx_usdc', 'research.identity.line : différent de celui du bloc research'];
        yield 'identité : début' => [['research', 'identity', 'decision_start_ms'], 1, 'research.identity.decision_start_ms : différent'];
        yield 'identité : fin' => [['research', 'identity', 'decision_end_ms'], 1, 'research.identity.decision_end_ms : différent'];
        yield 'identité : univers' => [['research', 'identity', 'universe_window_hash'], 'blake3:x', 'research.identity.universe_window_hash : différent'];
        yield 'identité : sous-ensemble' => [['research', 'identity', 'subset_hash'], 'blake3:x', 'research.identity.subset_hash : différent'];
        yield 'conservés différents de n_trades' => [['research', 'purge', 'trades_kept'], 10, 'research.purge.trades_kept : différent de n_trades'];
        yield 'purgés différents du csv' => [['research', 'purge', 'trades_purged'], 2, 'trades.csv : lignes : 10 trades pour 9 conservés et 2 purgés'];
        yield 'refus négatif' => [['research', 'funnel', 'entries_not_in_universe'], 'x', 'research.funnel.entries_not_in_universe doit être un entier'];
    }

    /**
     * @param list<string> $path
     */
    #[DataProvider('invalidResearchValues')]
    public function testInvalidResearchValuesAreRejected(array $path, mixed $value, string $message): void
    {
        $run = $this->copy();
        $run->setJson('metrics.json', $path, $value);

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage($message);

        $this->reader()->read($run->directory);
    }

    public function testAPurgedTradeMustBeRecognisedByItsExitType(): void
    {
        $run = $this->copy();
        $run->replace('trades.csv', 'end_of_window', 'strategy_exit');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('trades.csv : exit_type : 0 trades en end_of_window pour 1 purgés');

        $this->reader()->read($run->directory);
    }

    public function testAnOkxLineRequiresASubset(): void
    {
        $run = $this->copy();
        $run->setJson('metrics.json', ['research', 'line'], 'okx_usdc');
        $run->setJson('metrics.json', ['research', 'identity', 'line'], 'okx_usdc');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('research.subset : la ligne okx_usdc exige un sous-ensemble');

        $this->reader()->read($run->directory);
    }

    public function testAFullLineDoesNotCarryASubset(): void
    {
        $run = $this->copy();
        $run->setJson('metrics.json', ['research', 'subset'], ['kind' => 'okx_usdc_tradable', 'subset_hash' => 'blake3:x', 'legs_excluded' => []]);
        $run->setJson('metrics.json', ['research', 'identity', 'subset_hash'], 'blake3:x');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('research.subset : la ligne full ne porte pas de sous-ensemble');

        $this->reader()->read($run->directory);
    }

    public function testTheManifestBlockMustMatchTheMetrics(): void
    {
        $run = $this->copy();
        $run->setJson('manifest.json', ['data', 'block'], 'DEV');

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('manifest.json : data.block : différent de research.block des métriques');

        $this->reader()->read($run->directory);
    }

    public function testTheManifestDecisionWindowMustMatchTheMetrics(): void
    {
        $run = $this->copy();
        $run->setJson('manifest.json', ['data', 'decision_window', 'start_ms'], 1);

        $this->expectException(InvalidRunException::class);
        $this->expectExceptionMessage('manifest.json : data.decision_window : différent de research.decision_window des métriques');

        $this->reader()->read($run->directory);
    }

    public function testTheManifestMayOmitTheBlockAndTheInputs(): void
    {
        $run = $this->copy();
        $run->unsetJson('manifest.json', ['data', 'block']);
        $run->unsetJson('manifest.json', ['data', 'decision_window']);
        $run->unsetJson('manifest.json', ['research_inputs']);

        $report = $this->reader()->read($run->directory);

        self::assertNull($report->manifest->data->block);
        self::assertNull($report->manifest->data->decisionWindow);
        self::assertNull($report->manifest->researchInputs);
    }

    public function testTheBlockNamesAreTheFourBlocksOfTheProtocol(): void
    {
        self::assertSame(['DEV', 'VALIDATION', 'HOLDOUT', 'FORWARD'], ResearchInfo::BLOCKS);
        self::assertSame(['full', 'okx_usdc'], ResearchInfo::LINES);
        self::assertSame(['none', 'unguarded'], ResearchInfo::GUARDS);
    }
}
