<?php
namespace Acme\Tests;

require_once __DIR__ . '/../fixtures-global/Foo.php';
include dirname(__DIR__) . '/src/Support/functions.php';
require_once $dynamic . '/Nope.php';

use Acme\Service\UserService;

class FooTest
{
    public function test(): void
    {
        new \FooFixture();
        $name = UserService::class;
        new namespace\Local();
        new \acme\model\user();
    }
}
