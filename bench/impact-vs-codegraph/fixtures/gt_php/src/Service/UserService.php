<?php
namespace Acme\Service;

use Acme\Model\{User, Role as R};
use Acme\Model\HasName;
use function Acme\Support\helper;
use const Acme\Support\VERSION;

#[\Acme\Attr\Marker]
final class UserService extends Base implements \Acme\Contract\Greets
{
    use HasName;
    use Audits;

    private Clock $clock;

    private ?User $user = null;

    public function make(R $role, int $n): User|false
    {
        $table = new \Legacy_Db_Table();
        if ($table instanceof \Acme\Model\User) {
        }
        try {
        } catch (NotFound $e) {
        }
        $c = function () use ($table) {
            return 1;
        };
        /** @var \Acme\Model\Ghost $g */
        $s = 'Acme\Model\Ghost';
        return Helper::go();
    }
}
