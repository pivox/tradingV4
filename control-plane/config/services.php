<?php

declare(strict_types=1);

use Symfony\Component\DependencyInjection\Loader\Configurator\ContainerConfigurator;

return static function (ContainerConfigurator $container): void {
    $services = $container->services();

    $services->defaults()
        ->autowire()
        ->autoconfigure();

    // Les DTO sont construits par le lecteur seul : ils ne sont pas des services.
    $services->load('App\\', '../src/')
        ->exclude(['../src/Kernel.php', '../src/Run/Dto/']);
};
