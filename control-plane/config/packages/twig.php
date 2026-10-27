<?php

declare(strict_types=1);

use Symfony\Config\TwigConfig;

return static function (TwigConfig $twig): void {
    $twig->defaultPath('%kernel.project_dir%/templates');
    $twig->strictVariables(true);
    // Échappement HTML automatique : comportement par défaut de Twig, jamais désactivé ici.
};
