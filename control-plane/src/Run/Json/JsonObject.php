<?php

declare(strict_types=1);

namespace App\Run\Json;

use App\Run\InvalidRunException;
use JsonException;

/**
 * Accès typé à un objet JSON décodé, par chemin pointé (`engine.git_sha`).
 *
 * Toute clé absente ou de mauvais type lève une {@see InvalidRunException} qui porte le nom du
 * fichier et le chemin complet. Les clés en trop sont tolérées (compatibilité ascendante).
 */
final readonly class JsonObject
{
    /**
     * @param array<array-key, mixed> $data
     */
    private function __construct(
        private array $data,
        private string $file,
        private string $prefix,
    ) {
    }

    public static function decode(string $json, string $file): self
    {
        try {
            $decoded = json_decode($json, true, 512, JSON_THROW_ON_ERROR);
        } catch (JsonException $e) {
            throw InvalidRunException::invalidJson($file, $e->getMessage());
        }
        if (!is_array($decoded) || (array_is_list($decoded) && [] !== $decoded)) {
            throw InvalidRunException::wrongType($file, '$', 'un objet JSON', self::describe($decoded));
        }

        return new self($decoded, $file, '');
    }

    public function file(): string
    {
        return $this->file;
    }

    public function has(string $path): bool
    {
        return $this->lookup($path, false) !== null;
    }

    /**
     * Vrai si la clé est présente et vaut null ; une clé absente lève une exception.
     */
    public function isNull(string $path): bool
    {
        return null === $this->required($path);
    }

    public function string(string $path): string
    {
        $value = $this->required($path);
        if (!is_string($value)) {
            throw $this->wrongType($path, 'une chaîne', $value);
        }

        return $value;
    }

    public function nullableString(string $path): ?string
    {
        $value = $this->required($path);
        if (null !== $value && !is_string($value)) {
            throw $this->wrongType($path, 'une chaîne ou null', $value);
        }

        return $value;
    }

    public function int(string $path): int
    {
        $value = $this->required($path);
        if (!is_int($value)) {
            throw $this->wrongType($path, 'un entier', $value);
        }

        return $value;
    }

    public function nullableInt(string $path): ?int
    {
        $value = $this->required($path);
        if (null !== $value && !is_int($value)) {
            throw $this->wrongType($path, 'un entier ou null', $value);
        }

        return $value;
    }

    /**
     * Flottant ; un entier JSON est accepté (`300` pour `300.0`).
     */
    public function float(string $path): float
    {
        $value = $this->required($path);
        if (!is_int($value) && !is_float($value)) {
            throw $this->wrongType($path, 'un nombre', $value);
        }

        return (float) $value;
    }

    public function nullableFloat(string $path): ?float
    {
        $value = $this->required($path);
        if (null === $value) {
            return null;
        }
        if (!is_int($value) && !is_float($value)) {
            throw $this->wrongType($path, 'un nombre ou null', $value);
        }

        return (float) $value;
    }

    public function bool(string $path): bool
    {
        $value = $this->required($path);
        if (!is_bool($value)) {
            throw $this->wrongType($path, 'un booléen', $value);
        }

        return $value;
    }

    public function object(string $path): self
    {
        $value = $this->required($path);
        if (!is_array($value) || (array_is_list($value) && [] !== $value)) {
            throw $this->wrongType($path, 'un objet', $value);
        }

        return new self($value, $this->file, $this->qualify($path));
    }

    /**
     * Tableau JSON dont chaque élément est un objet.
     *
     * @return list<self>
     */
    public function objectList(string $path): array
    {
        $items = $this->rawList($path);
        $objects = [];
        foreach ($items as $index => $item) {
            if (!is_array($item) || (array_is_list($item) && [] !== $item)) {
                throw $this->wrongType(sprintf('%s[%d]', $path, $index), 'un objet', $item);
            }
            $objects[] = new self($item, $this->file, sprintf('%s[%d]', $this->qualify($path), $index));
        }

        return $objects;
    }

    /**
     * @return list<string>
     */
    public function stringList(string $path): array
    {
        $values = [];
        foreach ($this->rawList($path) as $index => $item) {
            if (!is_string($item)) {
                throw $this->wrongType(sprintf('%s[%d]', $path, $index), 'une chaîne', $item);
            }
            $values[] = $item;
        }

        return $values;
    }

    /**
     * @return list<int>
     */
    public function intList(string $path): array
    {
        $values = [];
        foreach ($this->rawList($path) as $index => $item) {
            if (!is_int($item)) {
                throw $this->wrongType(sprintf('%s[%d]', $path, $index), 'un entier', $item);
            }
            $values[] = $item;
        }

        return $values;
    }

    /**
     * Objet dont toutes les valeurs sont des entiers (compteurs par motif).
     *
     * @return array<string, int>
     */
    public function intMap(string $path): array
    {
        $map = [];
        foreach ($this->object($path)->data as $key => $item) {
            if (!is_int($item)) {
                throw $this->wrongType(sprintf('%s.%s', $path, (string) $key), 'un entier', $item);
            }
            $map[(string) $key] = $item;
        }

        return $map;
    }

    /**
     * Objet à valeurs scalaires ; une valeur composée est conservée sous forme de JSON compact.
     *
     * @return array<string, bool|int|float|string|null>
     */
    public function scalarMap(string $path): array
    {
        $map = [];
        foreach ($this->object($path)->data as $key => $item) {
            if (null === $item || is_scalar($item)) {
                $map[(string) $key] = $item;
                continue;
            }
            $map[(string) $key] = json_encode($item, JSON_UNESCAPED_SLASHES | JSON_UNESCAPED_UNICODE) ?: '';
        }

        return $map;
    }

    /**
     * @return list<mixed>
     */
    private function rawList(string $path): array
    {
        $value = $this->required($path);
        if (!is_array($value) || !array_is_list($value)) {
            throw $this->wrongType($path, 'un tableau', $value);
        }

        return $value;
    }

    private function required(string $path): mixed
    {
        $found = $this->lookup($path, true);
        if (null === $found) {
            throw InvalidRunException::missingKey($this->file, $this->qualify($path));
        }

        return $found[0];
    }

    /**
     * @return array{0: mixed}|null Enveloppe d'une valeur éventuellement nulle, ou null si la clé manque.
     */
    private function lookup(string $path, bool $strict): ?array
    {
        $current = $this->data;
        $segments = explode('.', $path);
        $last = count($segments) - 1;
        foreach ($segments as $position => $segment) {
            if (!array_key_exists($segment, $current)) {
                return null;
            }
            $value = $current[$segment];
            if ($position === $last) {
                return [$value];
            }
            if (!is_array($value)) {
                if ($strict) {
                    throw $this->wrongType(implode('.', array_slice($segments, 0, $position + 1)), 'un objet', $value);
                }

                return null;
            }
            $current = $value;
        }

        return null;
    }

    private function qualify(string $path): string
    {
        return '' === $this->prefix ? $path : $this->prefix . '.' . $path;
    }

    private function wrongType(string $path, string $expected, mixed $found): InvalidRunException
    {
        return InvalidRunException::wrongType($this->file, $this->qualify($path), $expected, self::describe($found));
    }

    private static function describe(mixed $value): string
    {
        return match (true) {
            null === $value => 'null',
            is_bool($value) => 'un booléen',
            is_int($value) => 'un entier',
            is_float($value) => 'un flottant',
            is_string($value) => 'une chaîne',
            is_array($value) => array_is_list($value) ? 'un tableau' : 'un objet',
            default => get_debug_type($value),
        };
    }
}
