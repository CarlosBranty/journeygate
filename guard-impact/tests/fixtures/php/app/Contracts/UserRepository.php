<?php

namespace App\Contracts;

use App\Models\User;

interface UserRepository
{
    public function find(int $id): ?User;
}
