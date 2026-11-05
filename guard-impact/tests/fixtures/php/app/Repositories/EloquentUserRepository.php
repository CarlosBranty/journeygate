<?php

namespace App\Repositories;

use App\Contracts\UserRepository;
use App\Models;

final class EloquentUserRepository implements UserRepository
{
    public function find(int $id): ?Models\User
    {
        return Models\User::query()->find($id);
    }
}
