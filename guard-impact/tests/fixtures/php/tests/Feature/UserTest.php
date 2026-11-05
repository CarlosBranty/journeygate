<?php

namespace Tests\Feature;

use App\Models\User;
use Tests\TestCase;

final class UserTest extends TestCase
{
    public function test_seeds(): void
    {
        $seeder = new \DatabaseSeeder();
        $user = new User();
    }
}
