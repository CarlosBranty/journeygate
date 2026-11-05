<?php

namespace App\Models;

use App\Enums\Status;
use App\Models\Concerns\HasRoles;
use Database\Factories\UserFactory;
use Illuminate\Database\Eloquent\Factories\HasFactory;
use Illuminate\Database\Eloquent\Model;

class User extends Model
{
    /** @use HasFactory<UserFactory> */
    use HasFactory, HasRoles;

    protected $casts = ['status' => Status::class];

    public function posts(): array
    {
        return [Post::class];
    }
}
