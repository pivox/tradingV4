<?php

declare(strict_types=1);

namespace App;

use Symfony\Bundle\FrameworkBundle\Kernel\MicroKernelTrait;
use Symfony\Component\HttpKernel\Kernel as BaseKernel;

/**
 * Noyau minimal du control plane (jalon 1) : configuration en PHP, aucun fichier `.env` ;
 * `bin/console` lit seulement `APP_ENV` et `APP_DEBUG` (voir `App\Console\KernelFactory`).
 */
final class Kernel extends BaseKernel
{
    use MicroKernelTrait;
}
