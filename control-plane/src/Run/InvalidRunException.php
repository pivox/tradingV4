<?php

declare(strict_types=1);

namespace App\Run;

use RuntimeException;

/**
 * Run illisible ou non conforme : le message nomme toujours le fichier, puis le chemin JSON ou la
 * ligne CSV en cause, puis la raison.
 */
final class InvalidRunException extends RuntimeException
{
    public static function directoryNotFound(string $directory): self
    {
        return new self(sprintf('dossier de run introuvable : %s', $directory));
    }

    public static function missingFile(string $file): self
    {
        return new self(sprintf('%s : fichier absent', $file));
    }

    public static function unreadableFile(string $file): self
    {
        return new self(sprintf('%s : fichier illisible', $file));
    }

    public static function invalidJson(string $file, string $reason): self
    {
        return new self(sprintf('%s : JSON invalide (%s)', $file, $reason));
    }

    public static function unknownSchema(string $file, string $found, string $expected): self
    {
        return new self(sprintf('%s : schéma inconnu : "%s" (attendu "%s")', $file, $found, $expected));
    }

    public static function missingKey(string $file, string $path): self
    {
        return new self(sprintf('%s : clé absente à %s', $file, $path));
    }

    public static function wrongType(string $file, string $path, string $expected, string $found): self
    {
        return new self(sprintf('%s : %s doit être %s, trouvé %s', $file, $path, $expected, $found));
    }

    public static function invalidValue(string $file, string $path, string $reason): self
    {
        return new self(sprintf('%s : %s : %s', $file, $path, $reason));
    }

    public static function unexpectedHeader(string $file, string $expected, string $found): self
    {
        return new self(sprintf('%s : en-tête inattendu (attendu "%s", trouvé "%s")', $file, $expected, $found));
    }

    public static function invalidCsvLine(string $file, int $line, string $reason): self
    {
        return new self(sprintf('%s : ligne %d : %s', $file, $line, $reason));
    }
}
