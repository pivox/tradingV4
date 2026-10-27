<?php

declare(strict_types=1);

namespace App\Report;

/** Écriture atomique du rapport : fichier temporaire dans le même dossier, puis `rename`. */
final class ReportWriter
{
    /**
     * @throws ReportWriteException
     */
    public function write(string $path, string $html): void
    {
        $directory = dirname($path);
        if (!is_dir($directory)) {
            throw ReportWriteException::directoryMissing($directory);
        }

        $temporary = @tempnam($directory, '.report-');
        if (false === $temporary) {
            throw ReportWriteException::writeFailed($path);
        }
        if (false === @file_put_contents($temporary, $html)) {
            @unlink($temporary);
            throw ReportWriteException::writeFailed($path);
        }
        // tempnam crée le fichier en 0600 ; un rapport se lit comme n'importe quel fichier du dossier.
        @chmod($temporary, 0o644);
        if (!@rename($temporary, $path)) {
            @unlink($temporary);
            throw ReportWriteException::writeFailed($path);
        }
    }
}
