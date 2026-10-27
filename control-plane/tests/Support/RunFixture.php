<?php

declare(strict_types=1);

namespace App\Tests\Support;

use RuntimeException;

/**
 * Copie du run d'exemple dans un dossier temporaire, modifiable fichier par fichier, pour fabriquer
 * des runs invalides sans toucher à la fixture versionnée.
 */
final class RunFixture
{
    public const SOURCE = __DIR__ . '/../fixtures/run-exemple';

    /** @var list<string> */
    public const FILES = ['manifest.json', 'metrics.json', 'trades.csv', 'equity.csv'];

    private function __construct(public readonly string $directory)
    {
    }

    public static function copy(): self
    {
        $directory = sys_get_temp_dir() . '/tv4-report-test-' . bin2hex(random_bytes(6));
        if (!mkdir($directory, 0o755, true)) {
            throw new RuntimeException('dossier temporaire impossible à créer');
        }
        foreach (self::FILES as $file) {
            copy(self::SOURCE . '/' . $file, $directory . '/' . $file);
        }

        return new self($directory);
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
        foreach (glob($this->directory . '/*') ?: [] as $file) {
            unlink($file);
        }
        rmdir($this->directory);
    }
}
