<?php

declare(strict_types=1);

namespace App\Tests\Support;

use RuntimeException;

/**
 * Copie d'un run d'exemple (s8 par défaut, le run portefeuille s9, le run de recherche s11, ou un dossier de profils s10
 * ou de lignes s11) dans un dossier
 * temporaire, modifiable fichier par fichier, pour fabriquer des runs invalides sans toucher à la fixture versionnée.
 * Les chemins d'un dossier de profils s'écrivent avec leur sous-dossier (`baseline/metrics.json`).
 */
final class RunFixture
{
    public const SOURCE = __DIR__ . '/../fixtures/run-exemple';

    /** Run portefeuille écrit à la main selon docs/specs/s9.md §13 (schémas s9). */
    public const PORTFOLIO_SOURCE = __DIR__ . '/../fixtures/run-portefeuille';

    /** Dossier de profils écrit à la main selon docs/specs/s10.md §12 et §13 (`profiles.json` et trois runs s10). */
    public const PROFILES_SOURCE = __DIR__ . '/../fixtures/run-profils';

    /** Run de recherche écrit à la main selon docs/specs/s11.md §9.2 (schémas s11, bloc FORWARD, ligne full). */
    public const RESEARCH_SOURCE = __DIR__ . '/../fixtures/run-recherche';

    /** Dossier de lignes écrit à la main selon docs/specs/s11.md §9.3 (`lines.json`, `full/` et `okx_usdc/`, deux runs s11). */
    public const LINES_SOURCE = __DIR__ . '/../fixtures/run-lignes';

    /** Dossier de lignes dont chaque ligne est un dossier de profils de coûts (trois runs s11 par ligne). */
    public const LINES_PROFILES_SOURCE = __DIR__ . '/../fixtures/run-lignes-profils';

    /** @var list<string> */
    public const FILES = ['manifest.json', 'metrics.json', 'trades.csv', 'equity.csv'];

    private function __construct(public readonly string $directory)
    {
    }

    public static function copy(string $source = self::SOURCE): self
    {
        $directory = sys_get_temp_dir() . '/tv4-report-test-' . bin2hex(random_bytes(6));
        if (!mkdir($directory, 0o755, true)) {
            throw new RuntimeException('dossier temporaire impossible à créer');
        }
        foreach (self::FILES as $file) {
            copy($source . '/' . $file, $directory . '/' . $file);
        }

        return new self($directory);
    }

    public static function copyPortfolio(): self
    {
        return self::copy(self::PORTFOLIO_SOURCE);
    }

    /** Copie du dossier de profils : `profiles.json` et les sous-dossiers `baseline/`, `adverse/` et `maker/`. */
    public static function copyProfiles(string $source = self::PROFILES_SOURCE): self
    {
        $directory = sys_get_temp_dir() . '/tv4-report-test-' . bin2hex(random_bytes(6));
        $run = new self($directory);
        $run->copyTree($source, $directory);

        return $run;
    }

    /** Copie d'un dossier de lignes : `lines.json`, `full/` et `okx_usdc/`, chacun un run ou un dossier de profils. */
    public static function copyLines(string $source = self::LINES_SOURCE): self
    {
        return self::copyProfiles($source);
    }

    private function copyTree(string $source, string $target): void
    {
        if (!is_dir($target) && !mkdir($target, 0o755, true)) {
            throw new RuntimeException('dossier temporaire impossible à créer : ' . $target);
        }
        foreach (scandir($source) ?: [] as $entry) {
            if ('.' === $entry || '..' === $entry || 'README.md' === $entry) {
                continue;
            }
            if (is_dir($source . '/' . $entry)) {
                $this->copyTree($source . '/' . $entry, $target . '/' . $entry);
            } elseif (!copy($source . '/' . $entry, $target . '/' . $entry)) {
                throw new RuntimeException('copie impossible : ' . $source . '/' . $entry);
            }
        }
    }

    /**
     * Copie `metrics.json`, `trades.csv` et `equity.csv` d'un dossier qui n'a pas de manifeste (le golden de tv4)
     * et écrit le `manifest.json` fourni.
     *
     * @param array<string, mixed> $manifest
     */
    public static function copyWithManifest(string $source, array $manifest): self
    {
        $directory = sys_get_temp_dir() . '/tv4-report-test-' . bin2hex(random_bytes(6));
        if (!mkdir($directory, 0o755, true)) {
            throw new RuntimeException('dossier temporaire impossible à créer');
        }
        $run = new self($directory);
        foreach (['metrics.json', 'trades.csv', 'equity.csv'] as $file) {
            if (!copy($source . '/' . $file, $directory . '/' . $file)) {
                $run->cleanup();
                throw new RuntimeException('copie impossible : ' . $source . '/' . $file);
            }
        }
        $run->write('manifest.json', json_encode($manifest, JSON_THROW_ON_ERROR | JSON_PRETTY_PRINT));

        return $run;
    }

    public function path(string $file): string
    {
        return $this->directory . '/' . $file;
    }

    public function read(string $file): string
    {
        $content = file_get_contents($this->path($file));
        if (false === $content) {
            throw new RuntimeException('lecture impossible : ' . $file);
        }

        return $content;
    }

    public function write(string $file, string $content): void
    {
        file_put_contents($this->path($file), $content);
    }

    /**
     * Remplace la valeur JSON d'un chemin (`['instruments', 'source']`) ; la clé peut ne pas exister.
     *
     * @param list<string> $path
     */
    public function setJson(string $file, array $path, mixed $value): void
    {
        $this->encode($file, self::withValue($this->decode($file), $path, $value, $file));
    }

    /**
     * Supprime une clé JSON (`['distribution', 'by_pair']`), ou échoue si elle n'existe pas.
     *
     * @param list<string> $path
     */
    public function unsetJson(string $file, array $path): void
    {
        $this->encode($file, self::withoutKey($this->decode($file), $path, $file));
    }

    /**
     * @param array<array-key, mixed> $node
     * @param list<string>            $path
     *
     * @return array<array-key, mixed>
     */
    private static function withValue(array $node, array $path, mixed $value, string $file): array
    {
        $key = array_shift($path);
        if (null === $key) {
            throw new RuntimeException($file . ' : chemin JSON vide');
        }
        if ([] === $path) {
            $node[$key] = $value;

            return $node;
        }
        $child = $node[$key] ?? null;
        if (!is_array($child)) {
            throw new RuntimeException(sprintf('%s : objet attendu à %s', $file, $key));
        }
        $node[$key] = self::withValue($child, $path, $value, $file);

        return $node;
    }

    /**
     * @param array<array-key, mixed> $node
     * @param list<string>            $path
     *
     * @return array<array-key, mixed>
     */
    private static function withoutKey(array $node, array $path, string $file): array
    {
        $key = array_shift($path);
        if (null === $key || !array_key_exists($key, $node)) {
            throw new RuntimeException(sprintf('%s : clé JSON absente : %s', $file, $key ?? ''));
        }
        if ([] === $path) {
            unset($node[$key]);

            return $node;
        }
        $child = $node[$key];
        if (!is_array($child)) {
            throw new RuntimeException(sprintf('%s : objet attendu à %s', $file, $key));
        }
        $node[$key] = self::withoutKey($child, $path, $file);

        return $node;
    }

    /**
     * @return array<array-key, mixed>
     */
    private function decode(string $file): array
    {
        $data = json_decode($this->read($file), true, 512, JSON_THROW_ON_ERROR);
        if (!is_array($data)) {
            throw new RuntimeException($file . ' : objet JSON attendu');
        }

        return $data;
    }

    /**
     * @param array<array-key, mixed> $data
     */
    private function encode(string $file, array $data): void
    {
        $this->write($file, json_encode($data, JSON_THROW_ON_ERROR));
    }

    public function remove(string $file): void
    {
        unlink($this->path($file));
    }

    /** Remplace exactement une occurrence, ou échoue : un test ne doit jamais muter dans le vide. */
    public function replace(string $file, string $search, string $replace): void
    {
        $content = $this->read($file);
        if (1 !== substr_count($content, $search)) {
            throw new RuntimeException(sprintf('"%s" doit apparaître une fois dans %s', $search, $file));
        }
        $this->write($file, str_replace($search, $replace, $content));
    }

    public function cleanup(): void
    {
        $this->removeTree($this->directory);
    }

    private function removeTree(string $directory): void
    {
        foreach (scandir($directory) ?: [] as $name) {
            if ('.' === $name || '..' === $name) {
                continue;
            }
            $entry = $directory . '/' . $name;
            is_dir($entry) ? $this->removeTree($entry) : unlink($entry);
        }
        rmdir($directory);
    }
}
