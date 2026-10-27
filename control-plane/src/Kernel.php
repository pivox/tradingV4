<?php

declare(strict_types=1);

namespace App;

use Symfony\Bundle\FrameworkBundle\Kernel\MicroKernelTrait;
use Symfony\Component\HttpKernel\Kernel as BaseKernel;

/**
 * Noyau minimal du control plane (jalon 1) : configuration en PHP, aucune variable d'environnement.
 */
final class Kernel extends BaseKernel
{
    use MicroKernelTrait;
}
