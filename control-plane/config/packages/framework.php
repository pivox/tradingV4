<?php

declare(strict_types=1);

use Symfony\Config\FrameworkConfig;

return static function (FrameworkConfig $framework, string $env): void {
    $framework->httpMethodOverride(false);
    $framework->handleAllThrowables(true);
    // Ni session, ni CSRF, ni secret : le jalon 1 n'expose aucun service HTTP.
    $framework->phpErrors()->log(true);

    if ('test' === $env) {
        $framework->test(true);
    }
};
