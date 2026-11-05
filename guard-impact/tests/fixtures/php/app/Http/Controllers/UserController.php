<?php

namespace App\Http\Controllers;

use App\Attributes\Audited;
use App\Contracts\UserRepository;
use App\Enums\Status;
use App\Exceptions\UserNotFound;
use App\Models\{User, Post as Article};
use Illuminate\Http\Request;
use function App\Support\money;
use const App\Support\CURRENCY;

class UserController extends Controller
{
    public function __construct(private UserRepository $users)
    {
    }

    #[Audited]
    public function show(Request $request, int $id): string
    {
        try {
            $user = $this->users->find($id) ?? throw new UserNotFound();
        } catch (UserNotFound | \InvalidArgumentException $e) {
            return 'missing';
        }
        if ($user instanceof User && $user->status === Status::Active) {
            return money(100) . CURRENCY . Article::class;
        }
        return 'inactive';
    }
}
