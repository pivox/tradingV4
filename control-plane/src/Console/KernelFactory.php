<?php

declare(strict_types=1);

namespace App\Console;

use App\Kernel;
use Symfony\Component\Console\Input\InputInterface;

/**
 * Choisit l'environnement et le mode debug du noyau comme le `bin/console` de Symfony d'avant Runtime :
 * `--env` / `-e` (défaut : `APP_ENV`, sinon `prod`) ; debug actif tant que `APP_DEBUG` est absent
 * ou vrai, éteint par `APP_DEBUG=0` ou par `--no-debug`.
 *
 * Le debug est actif par défaut pour une raison de fraîcheur : en debug, Symfony vérifie les ressources
 * du conteneur (configuration, services) et le reconstruit après un `git pull`. Sans cela, un cache
 * `var/cache/prod` périmé rendrait un rapport d'une version antérieure du code.
 */
final class KernelFactory
{
    /**
     * @param array<array-key, mixed> $server variables d'environnement (`$_SERVER`)
     */
    public static function environment(InputInterface $input, array $server): string
    {
        $default = $server['APP_ENV'] ?? 'prod';
        $env = $input->getParameterOption(['--env', '-e'], is_string($default) ? $default : 'prod', true);

        return is_string($env) && '' !== $env ? $env : 'prod';
    }

    /**
     * @param array<array-key, mixed> $server variables d'environnement (`$_SERVER`)
     */
    public static function debug(InputInterface $input, array $server): bool
    {
        if ($input->hasParameterOption('--no-debug', true)) {
            return false;
        }

        return filter_var($server['APP_DEBUG'] ?? true, FILTER_VALIDATE_BOOL);
    }

    /**
     * @param array<array-key, mixed> $server variables d'environnement (`$_SERVER`)
     */
    public static function create(InputInterface $input, array $server): Kernel
    {
        return new Kernel(self::environment($input, $server), self::debug($input, $server));
    }
}
