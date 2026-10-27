<?php

declare(strict_types=1);

namespace App\Report;

use RuntimeException;

/** Le rapport n'a pas pu être écrit sur disque. */
final class ReportWriteException extends RuntimeException
{
    public static function directoryMissing(string $directory): self
    {
        return new self(sprintf('dossier de sortie introuvable : %s', $directory));
    }

    public static function writeFailed(string $path): self
    {
        return new self(sprintf('écriture impossible : %s', $path));
    }
}
