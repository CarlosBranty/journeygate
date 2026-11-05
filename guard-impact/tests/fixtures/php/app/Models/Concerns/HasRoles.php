<?php

namespace App\Models\Concerns;

trait HasRoles
{
    public function isAdmin(): bool
    {
        return false;
    }
}
