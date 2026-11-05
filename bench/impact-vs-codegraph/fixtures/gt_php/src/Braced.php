<?php
namespace Acme\X {
    use Acme\Model\User as U;

    function f(U $u) {}
}

namespace Acme\Y {
    function g(U $u) {}

    // Classes named only by a parameter attribute and by default values.
    function h(#[\Acme\Attr\Marker] $a, $b = new \Acme\Service\Clock(), $c = \Acme\Model\Role::Admin) {}
}
