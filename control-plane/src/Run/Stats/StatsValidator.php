<?php

declare(strict_types=1);

namespace App\Run\Stats;

use App\Run\Dto\StatsReport;
use App\Run\InvalidRunException;
use JsonException;
use stdClass;

/**
 * Validation exhaustive des types JSON ; objets et listes restent distincts au décodage.
 * @phpstan-import-type StatsData from StatsReport
 */
final class StatsValidator
{
    public static function decode(string $json): StatsReport
    {
        try {
            $value = json_decode($json, false, 512, JSON_THROW_ON_ERROR);
        } catch (JsonException $e) {
            throw InvalidRunException::invalidJson('stats.json', $e->getMessage());
        }
        $validator = new self();
        /** @var StatsData $data */
        $data = $validator->validate($value, 'StatsData', '$');

        return new StatsReport($data);
    }

    private function validate(mixed $value, string $type, string $path): mixed
    {
        if (str_starts_with($type, '?')) {
            return null === $value ? null : $this->validate($value, substr($type, 1), $path);
        }
        if (str_ends_with($type, '[]')) {
            self::require(is_array($value) && array_is_list($value), $path, 'tableau JSON requis');
            assert(is_array($value));
            $result = [];
            foreach ($value as $i => $item) {
                $result[] = $this->validate($item, substr($type, 0, -2), $path . '.' . $i);
            }
            return $result;
        }
        if (isset(StatsSchema::OBJECTS[$type])) {
            self::require($value instanceof stdClass, $path, 'objet JSON requis');
            assert($value instanceof stdClass);
            $fields = StatsSchema::OBJECTS[$type];
            self::require([] === array_diff(array_keys(get_object_vars($value)), array_keys($fields)), $path, 'champ inconnu');
            $result = [];
            foreach ($fields as $key => $fieldType) {
                self::require(property_exists($value, $key), $path . '.' . $key, 'champ obligatoire absent');
                $result[$key] = $this->validate($value->{$key}, $fieldType, $path . '.' . $key);
            }
            StatsInvariants::check($type, $result, $path);
            return $result;
        }
        if (isset(StatsSchema::ENUMS[$type])) {
            self::require(in_array($value, StatsSchema::ENUMS[$type], true), $path, 'valeur hors énumération ' . $type);
            return $value;
        }
        if (in_array($type, ['number', 'positive', 'nonnegative', 'probability', 'return'], true)) {
            self::require((is_int($value) || is_float($value)) && is_finite((float) $value), $path, 'nombre fini requis');
            self::require('positive' !== $type || $value > 0, $path, 'nombre strictement positif requis');
            self::require(!in_array($type, ['nonnegative', 'probability'], true) || $value >= 0, $path, 'nombre négatif');
            self::require('probability' !== $type || $value <= 1, $path, 'probabilité hors [0,1]');
            self::require('return' !== $type || $value >= -1, $path, 'rendement inférieur à -1');
            return $value;
        }
        if (in_array($type, ['int', 'uint'], true)) {
            self::require(is_int($value) && ('uint' !== $type || $value >= 0), $path, 'entier requis dans la capacité PHP');
            return $value;
        }
        if ('bool' === $type) {
            self::require(is_bool($value), $path, 'booléen requis');
            return $value;
        }
        self::require(is_string($value), $path, 'chaîne requise');
        assert(is_string($value));
        if ('seed' === $type) {
            self::require(
                1 === preg_match('/\A(?:0|[1-9][0-9]*)\z/D', $value)
                && (strlen($value) < 20 || (strlen($value) === 20 && strcmp($value, '18446744073709551615') <= 0)),
                $path,
                'graine u64 décimale canonique requise sous forme de chaîne',
            );
        } elseif (in_array($type, ['hex', 'hash'], true)) {
            $pattern = 'hash' === $type ? '/\Ablake3:[0-9a-f]{64}\z/D' : '/\A[0-9a-f]{64}\z/D';
            self::require(1 === preg_match($pattern, $value), $path, 'empreinte invalide');
        } else {
            self::require('string' === $type && '' !== trim($value), $path, 'chaîne non vide requise');
        }
        return $value;
    }

    /** Comparaison des seuls champs numériques, avec la tolérance de sérialisation S8. */
    public static function sameNumber(float|int $left, float|int $right): bool
    {
        return is_finite((float) $left) && is_finite((float) $right)
            && abs($left - $right) <= 20 * PHP_FLOAT_EPSILON * max(abs($left), abs($right), PHP_FLOAT_MIN);
    }

    public static function require(bool $condition, string $path, string $reason): void
    {
        if (!$condition) {
            throw InvalidRunException::invalidValue('stats.json', $path, $reason);
        }
    }
}
