<?php

require __DIR__ . '/../routes/web.php';

return new \App\Http\Controllers\UserController(
    new App\Repositories\EloquentUserRepository()
);
