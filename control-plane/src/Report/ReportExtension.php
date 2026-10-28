<?php

declare(strict_types=1);

namespace App\Report;

use Twig\Extension\AbstractExtension;
use Twig\TwigFilter;

/** Filtres Twig de mise en forme du rapport ; toute la logique est dans {@see Format}. */
final class ReportExtension extends AbstractExtension
{
    public function __construct(private readonly Format $format)
    {
    }

    public function getFilters(): array
    {
        return [
            new TwigFilter('money', $this->format->money(...)),
            new TwigFilter('qty', $this->format->quantity(...)),
            new TwigFilter('pct', $this->format->percent(...)),
            new TwigFilter('int', $this->format->integer(...)),
            new TwigFilter('num', $this->format->number(...)),
            new TwigFilter('datetime', $this->format->dateTime(...)),
            new TwigFilter('duration', $this->format->duration(...)),
            new TwigFilter('scalar', $this->format->scalar(...)),
            new TwigFilter('yesno', $this->format->yesNo(...)),
        ];
    }
}
