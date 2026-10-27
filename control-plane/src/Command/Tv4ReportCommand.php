<?php

declare(strict_types=1);

namespace App\Command;

use App\Report\ReportRenderer;
use App\Report\ReportWriteException;
use App\Report\ReportWriter;
use App\Run\InvalidRunException;
use App\Run\RunDirectoryReader;
use Symfony\Component\Console\Attribute\AsCommand;
use Symfony\Component\Console\Command\Command;
use Symfony\Component\Console\Input\InputArgument;
use Symfony\Component\Console\Input\InputInterface;
use Symfony\Component\Console\Input\InputOption;
use Symfony\Component\Console\Output\OutputInterface;
use Symfony\Component\Console\Style\SymfonyStyle;

/**
 * `tv4:report <run_dir> [--output <fichier.html>]` : rend le rapport HTML statique d'un run produit
 * par `tv4`. Codes : 0 rendu écrit, 1 run invalide ou écriture impossible, 2 usage (Symfony).
 */
#[AsCommand(name: 'tv4:report', description: "Rend le rapport HTML statique d'un run de tv4")]
final class Tv4ReportCommand extends Command
{
    public function __construct(
        private readonly RunDirectoryReader $reader,
        private readonly ReportRenderer $renderer,
        private readonly ReportWriter $writer,
    ) {
        parent::__construct();
    }

    protected function configure(): void
    {
        $this
            ->addArgument('run_dir', InputArgument::REQUIRED, 'Dossier du run (manifest.json, metrics.json, trades.csv, equity.csv)')
            ->addOption('output', 'o', InputOption::VALUE_REQUIRED, 'Fichier HTML à écrire (défaut : <run_dir>/report.html)');
    }

    protected function execute(InputInterface $input, OutputInterface $output): int
    {
        $io = new SymfonyStyle($input, $output);
        $runDir = $input->getArgument('run_dir');
        $option = $input->getOption('output');
        if (!is_string($runDir)) {
            $io->error('Argument run_dir manquant.');

            return Command::FAILURE;
        }
        $target = is_string($option) && '' !== $option ? $option : rtrim($runDir, '/\\') . '/report.html';

        try {
            $html = $this->renderer->render($this->reader->read($runDir));
            $this->writer->write($target, $html);
        } catch (InvalidRunException $e) {
            $io->error('Run invalide : ' . $e->getMessage());

            return Command::FAILURE;
        } catch (ReportWriteException $e) {
            $io->error('Rapport non écrit : ' . $e->getMessage());

            return Command::FAILURE;
        }

        $io->success(sprintf('Rapport écrit dans %s (%d octets).', $target, strlen($html)));

        return Command::SUCCESS;
    }
}
