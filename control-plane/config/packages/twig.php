<?php

declare(strict_types=1);

use Symfony\Config\TwigConfig;

return static function (TwigConfig $twig): void {
    $twig->defaultPath('%kernel.project_dir%/templates');
    $twig->strictVariables(true);
    // Pas de cache Twig compilé : le rapport ne peut jamais refléter un template antérieur à un git pull.
    // Le coût (compilation à chaque lancement) est négligeable pour une commande ponctuelle.
    $twig->cache(false);
    // Échappement HTML automatique : comportement par défaut de Twig, jamais désactivé ici.
};
