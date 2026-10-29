<?php

declare(strict_types=1);

namespace App\Run;

use App\Run\Dto\MakerStatus;
use App\Run\Dto\ProfileGates;
use App\Run\Dto\ProfilesReport;
use App\Run\Dto\ProfileSummary;
use App\Run\Dto\RunReport;
use App\Run\Json\JsonObject;

/**
 * Lit un dossier de profils de coûts (docs/specs/s10.md §12.4) : `profiles.json` (schéma `tv4-profiles-v1`) et les
 * trois sous-dossiers `baseline/`, `adverse/` et `maker/`, chacun lu comme un run s10 par {@see RunDirectoryReader}.
 *
 * Aucun hash ni aucune étiquette n'est recalculé. Le lecteur recoupe seulement ce que `profiles.json` répète du
 * manifeste et des métriques de chaque run (identifiants, hashes, rôle, profil, capital, graine, grille, effectif) : un écart
 * veut dire que le fichier ne décrit pas ces sous-dossiers.
 */
final class ProfilesDirectoryReader
{
    public const FILE = 'profiles.json';
    public const SCHEMA = 'tv4-profiles-v1';

    /** Les trois profils, dans l'ordre fixe du moteur. @var list<string> */
    public const PROFILE_NAMES = ['baseline', 'adverse', 'maker'];

    /** @var list<string> */
    public const LABELS = ['indeterminate', 'passes_costs', 'fill_dependent', 'cost_fragile', 'negative'];

    /** @var list<string> */
    public const MAKER_STATUSES = ['too_few_trades', 'non_positive', 'positive_mostly_taker', 'positive'];

    /** @var list<string> */
    public const ROLES = ['gate', 'informative'];

    public function __construct(private readonly RunDirectoryReader $runs)
    {
    }

    /** Un dossier qui contient `profiles.json` est un dossier de profils. */
    public static function isProfilesDirectory(string $directory): bool
    {
        return is_file(rtrim($directory, '/\\') . '/' . self::FILE);
    }

    /**
     * @throws InvalidRunException
     */
    public function read(string $directory): ProfilesReport
    {
        if (!is_dir($directory)) {
            throw InvalidRunException::directoryNotFound($directory);
        }
        $dir = rtrim($directory, '/\\');
        $content = is_file($dir . '/' . self::FILE) ? @file_get_contents($dir . '/' . self::FILE) : false;
        if (false === $content) {
            throw InvalidRunException::missingFile(self::FILE);
        }
        $j = JsonObject::decode($content, self::FILE);
        if (!$j->has('schema')) {
            throw InvalidRunException::missingKey(self::FILE, 'schema');
        }
        if (self::SCHEMA !== $j->string('schema')) {
            throw InvalidRunException::unknownSchema(self::FILE, $j->string('schema'), [self::SCHEMA]);
        }

        $summaries = [];
        foreach ($j->objectList('profiles') as $index => $o) {
            $summaries[] = $this->summary($o, $index);
        }
        $names = array_map(static fn (ProfileSummary $p): string => $p->name, $summaries);
        $sorted = $names;
        sort($sorted);
        $expected = self::PROFILE_NAMES;
        sort($expected);
        if ($sorted !== $expected) {
            throw InvalidRunException::invalidValue(
                self::FILE,
                'profiles',
                sprintf(
                    'trois profils attendus (%s), trouvé : %s',
                    implode(', ', self::PROFILE_NAMES),
                    [] === $names ? 'aucun' : implode(', ', $names),
                ),
            );
        }
        if ($names !== self::PROFILE_NAMES) {
            throw InvalidRunException::invalidValue(
                self::FILE,
                'profiles',
                sprintf('ordre %s attendu, trouvé : %s', implode(', ', self::PROFILE_NAMES), implode(', ', $names)),
            );
        }

        return new ProfilesReport(
            profilesId: $j->string('profiles_id'),
            datasetHash: $j->string('dataset_hash'),
            initialCash: $j->float('initial_cash'),
            seed: $j->int('seed'),
            feeGrid: $j->string('fee_grid'),
            costsFileHash: $j->string('costs_file_hash'),
            costMeasuresHash: $j->has('cost_measures_hash') ? $j->nullableString('cost_measures_hash') : null,
            stressCalibrated: $j->bool('stress_calibrated'),
            profiles: $summaries,
            maker: $this->maker($j->object('maker')),
            gates: $this->gates($j->object('gates')),
            label: $this->oneOf($j, 'label', self::LABELS),
            labelFr: $j->string('label_fr'),
            runs: $this->readRuns($dir, $summaries, $j),
        );
    }

    /**
     * @param list<ProfileSummary> $summaries
     *
     * @return array<string, RunReport>
     */
    private function readRuns(string $dir, array $summaries, JsonObject $j): array
    {
        $runs = [];
        foreach ($summaries as $index => $summary) {
            $run = $this->runs->read($dir . '/' . $summary->dir);
            $this->check($j, $summary, $index, $run);
            $runs[$summary->name] = $run;
        }

        return $runs;
    }

    private function summary(JsonObject $o, int $index): ProfileSummary
    {
        $dir = $o->string('dir');
        if (1 !== preg_match('/^[A-Za-z0-9_-]+$/', $dir)) {
            throw InvalidRunException::invalidValue(
                self::FILE,
                sprintf('profiles[%d].dir', $index),
                sprintf('nom de sous-dossier simple attendu, trouvé "%s"', $dir),
            );
        }

        return new ProfileSummary(
            name: $this->oneOf($o, 'name', self::PROFILE_NAMES, sprintf('profiles[%d].name', $index)),
            dir: $dir,
            role: $this->oneOf($o, 'role', self::ROLES, sprintf('profiles[%d].role', $index)),
            runId: $o->string('run_id'),
            configHash: $o->string('config_hash'),
            resultHash: $o->string('result_hash'),
            verdict: $o->string('verdict'),
            nTrades: $o->int('n_trades'),
            expectancyNet: $o->nullableFloat('expectancy_net'),
            expectancyR: $o->nullableFloat('expectancy_r'),
            pnlNet: $o->float('pnl_net'),
            roundTripCostRateMean: $o->nullableFloat('round_trip_cost_rate_mean'),
            costRMean: $o->nullableFloat('cost_r_mean'),
            costRPass: $this->nullableBool($o, 'cost_r_pass'),
            costThirdPass: $o->bool('cost_third_pass'),
            grossMedianRate: $o->nullableFloat('gross_median_rate'),
            positive: $o->bool('positive'),
        );
    }

    private function maker(JsonObject $o): MakerStatus
    {
        return new MakerStatus(
            status: $this->oneOf($o, 'status', self::MAKER_STATUSES),
            nTrades: $o->int('n_trades'),
            expectancyNet: $o->nullableFloat('expectancy_net'),
            entriesConverted: $o->int('entries_converted'),
            entriesTakenAtArrival: $o->int('entries_taken_at_arrival'),
            entriesFilledMaker: $o->int('entries_filled_maker'),
            makerEntryMakerShare: $o->nullableFloat('maker_entry_maker_share'),
            minMakerEntryShare: $o->float('min_maker_entry_share'),
        );
    }

    private function gates(JsonObject $o): ProfileGates
    {
        return new ProfileGates(
            costRBaseline: $this->nullableBool($o, 'cost_r.baseline'),
            costRAdverse: $this->nullableBool($o, 'cost_r.adverse'),
            costThirdBaseline: $o->bool('cost_third.baseline'),
            costThirdAdverse: $o->bool('cost_third.adverse'),
            grossMedianVs2xBaselineCost: $this->nullableBool($o, 'gross_median_vs_2x_baseline_cost'),
            bothGateProfilesPositive: $o->bool('both_gate_profiles_positive'),
        );
    }

    /**
     * Recoupe `profiles.json` avec le run du profil : ce que le fichier répète doit être ce que le run a écrit.
     */
    private function check(JsonObject $j, ProfileSummary $summary, int $index, RunReport $run): void
    {
        $at = static fn (string $key): string => sprintf('profiles[%d].%s', $index, $key);
        $manifest = $run->manifest;
        $metrics = $run->metrics;
        $costs = $metrics->costs;
        if (null === $costs) {
            throw InvalidRunException::invalidValue(self::FILE, $at('dir'), sprintf('"%s" n\'est pas un run s10', $summary->dir));
        }

        $checks = [
            [$at('name'), $summary->name, $costs->profile, 'le run porte le profil "%2$s", pas "%1$s"'],
            [$at('run_id'), $summary->runId, $manifest->runId, 'différent de celui du manifeste de %s'],
            [$at('config_hash'), $summary->configHash, $manifest->configHash, 'différent de celui du manifeste de %s'],
            [$at('result_hash'), $summary->resultHash, $manifest->resultHash, 'différent de celui du manifeste de %s'],
            [$at('verdict'), $summary->verdict, $metrics->verdict, 'différent de celui des métriques de %s'],
        ];
        foreach ($checks as [$path, $found, $expected, $reason]) {
            if ($found !== $expected) {
                throw InvalidRunException::invalidValue(self::FILE, $path, sprintf($reason, $summary->name, $expected));
            }
        }
        if ($summary->nTrades !== $metrics->nTrades) {
            throw InvalidRunException::invalidValue(self::FILE, $at('n_trades'), 'différent de celui des métriques de ' . $summary->name);
        }

        // Rôle attendu par profil (s10 §12.4) : porte pour baseline et adverse, informatif pour maker, et celui
        // que le run déclare dans `costs.gates_profile`.
        $expectedRole = 'maker' === $summary->name ? 'informative' : 'gate';
        if ($summary->role !== $expectedRole) {
            throw InvalidRunException::invalidValue(
                self::FILE,
                $at('role'),
                sprintf('le profil %s attend le rôle "%s"', $summary->name, $expectedRole),
            );
        }
        if ($summary->isGate() !== $costs->gatesProfile) {
            throw InvalidRunException::invalidValue(
                self::FILE,
                $at('role'),
                'différent de costs.gates_profile du run ' . $summary->name,
            );
        }

        $shared = [
            ['dataset_hash', $j->string('dataset_hash'), $manifest->datasetHash],
            ['initial_cash', $j->float('initial_cash'), $manifest->config->initialCash],
            ['costs_file_hash', $j->string('costs_file_hash'), $costs->costsFileHash],
            ['fee_grid', $j->string('fee_grid'), $costs->feeGrid],
            ['seed', $j->int('seed'), $costs->seed],
            ['stress_calibrated', $j->bool('stress_calibrated'), $costs->stressCalibrated],
            [
                'cost_measures_hash',
                $j->has('cost_measures_hash') ? $j->nullableString('cost_measures_hash') : null,
                $costs->costMeasuresHash,
            ],
        ];
        foreach ($shared as [$path, $found, $expected]) {
            if ($found !== $expected) {
                throw InvalidRunException::invalidValue(self::FILE, $path, 'différent de celui du run ' . $summary->name);
            }
        }
        if (null !== $manifest->profilesId && $manifest->profilesId !== $j->string('profiles_id')) {
            throw InvalidRunException::invalidValue(self::FILE, 'profiles_id', 'différent de celui du manifeste de ' . $summary->name);
        }
    }

    /**
     * @param list<string> $allowed
     */
    private function oneOf(JsonObject $o, string $path, array $allowed, ?string $display = null): string
    {
        $value = $o->string($path);
        if (!in_array($value, $allowed, true)) {
            throw InvalidRunException::invalidValue(
                self::FILE,
                $display ?? $path,
                sprintf('valeur inconnue "%s" (attendu : %s)', $value, implode(', ', $allowed)),
            );
        }

        return $value;
    }

    /** Booléen dont la clé est obligatoire et la valeur peut être nulle. */
    private function nullableBool(JsonObject $o, string $path): ?bool
    {
        return $o->isNull($path) ? null : $o->bool($path);
    }
}
