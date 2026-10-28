<?php

declare(strict_types=1);

namespace App\Console;

use Symfony\Bundle\FrameworkBundle\Console\Application;
use Symfony\Component\Console\Exception\ExceptionInterface;
use Symfony\Component\Console\Input\InputInterface;
use Symfony\Component\Console\Output\ConsoleOutputInterface;
use Symfony\Component\Console\Output\OutputInterface;

/**
 * Application console du control plane. Elle sépare les codes de sortie : une erreur d'usage
 * (argument manquant, option inconnue, commande inconnue) sort avec 2, distinct du 1 d'un run invalide
 * ou d'une écriture impossible que la commande renvoie elle-même. Symfony sortirait 1 dans les deux cas.
 */
final class Tv4Application extends Application
{
    public const EXIT_USAGE = 2;

    public function doRun(InputInterface $input, OutputInterface $output): int
    {
        try {
            $this->ensureCommandExists($input);

            return parent::doRun($input, $output);
        } catch (ExceptionInterface $e) {
            $this->renderThrowable($e, $output instanceof ConsoleOutputInterface ? $output->getErrorOutput() : $output);

            return self::EXIT_USAGE;
        }
    }

    /**
     * Résout le nom de commande avant `parent::doRun()` : en terminal interactif, Symfony propose sinon
     * « Voulez-vous lancer X à la place ? » pour une commande inconnue et sort avec 1 si on refuse.
     */
    private function ensureCommandExists(InputInterface $input): void
    {
        try {
            $input->bind($this->getDefinition());
        } catch (ExceptionInterface) {
            // La validation complète des arguments vient plus tard, une fois la commande connue.
        }
        $name = $this->getCommandName($input);
        if (null !== $name && '' !== $name) {
            $this->find($name);
        }
    }
}
