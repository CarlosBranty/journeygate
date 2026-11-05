<?php

namespace Illuminate\Database\Eloquent;

abstract class Model
{
    public static function query(): static
    {
        return new static();
    }
}
