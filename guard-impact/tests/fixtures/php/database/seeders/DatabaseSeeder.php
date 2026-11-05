<?php

use App\Models\User;

class DatabaseSeeder
{
    public function run(): void
    {
        User::query();
    }
}
