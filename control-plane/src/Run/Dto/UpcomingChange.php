<?php

declare(strict_types=1);

namespace App\Run\Dto;

/** Changement à venir annoncé par OKX sur un instrument (`upc_chg`), recopié sans être appliqué au backtest. */
final readonly class UpcomingChange
{
    public function __construct(
        public string $instId,
        public string $param,
        public string $newValue,
        public string $effTime,
    ) {
    }

    /** Date d'effet en millisecondes si OKX l'a donnée sous cette forme, sinon null. */
    public function effTimeMs(): ?int
    {
        return ctype_digit($this->effTime) ? (int) $this->effTime : null;
    }
}
