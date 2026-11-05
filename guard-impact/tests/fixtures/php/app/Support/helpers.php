<?php

namespace App\Support;

const CURRENCY = 'EUR';

function money(int $cents): string
{
    return number_format($cents / 100, 2) . ' ' . CURRENCY;
}
