<?php
// PHP ground truth for the impact-vs-codegraph benchmark.
//
// usage: php gt_php.php <repo> <out.json>
//
// Every non-vendor .php file is tokenized with PHP's own lexer
// (PhpToken::tokenize). A small structural pass over the significant tokens
// implements PHP's compile-time name resolution: `namespace X;` / braced
// namespaces, `use` imports (plain, aliased, grouped, mixed groups,
// `use function`, `use const`), `namespace\X` and fully qualified names.
// Nothing is executed and no autoloader runs.
//
// Edge semantics (same as guard-impact's PHP plugin): file A -> file B when A
//   * names a class/interface/trait/enum in a class position: `new`, `extends`,
//     `implements`, `instanceof`, `insteadof`, `catch`, trait `use`,
//     `X::...` (calls, properties, constants, ::class), attributes `#[X]`
//     (parameter attributes too), parameter / return / property /
//     typed-constant types, and `new X` / `X::...` in parameter default
//     values (positions read inside function signatures); or a class `use`
//     import (the import itself names the class); the fully qualified name is
//     mapped to a file with the composer.json psr-4 / psr-0 maps (autoload and
//     autoload-dev, longest prefix first, the exact-case file must exist),
//     falling back to every file that declares the class (case-insensitive);
//   * imports with `use function` / `use const` a function / constant that B
//     declares; or
//   * statically includes B: require/include(_once) of a string literal
//     (relative to A's directory), or of __DIR__, dirname(__FILE__),
//     dirname(__DIR__[, n]) concatenated with a literal starting with `/`.
// Docblocks, strings, dynamic names (`new $c`), calls of namespaced functions
// without `use function` and `define()` are not edges. Semantic gap: no
// run-time or autoload execution, so the include_path / cwd, classmap-only
// autoloading, conditional declarations and container/facade wiring are not
// observed.

const NAME_IDS = [T_STRING, T_NAME_QUALIFIED, T_NAME_FULLY_QUALIFIED, T_NAME_RELATIVE];
const NOT_CLASSES = ['self', 'static', 'parent', 'int', 'float', 'bool', 'string', 'array', 'callable', 'iterable',
    'object', 'mixed', 'void', 'null', 'never', 'false', 'true'];
const MODIFIERS = [T_PUBLIC, T_PROTECTED, T_PRIVATE, T_VAR, T_STATIC, T_READONLY, T_ABSTRACT, T_FINAL,
    T_PUBLIC_SET, T_PROTECTED_SET, T_PRIVATE_SET];

[$_, $repoArg, $outPath] = $argv + [null, null, null];
if ($repoArg === null || $outPath === null) {
    fwrite(STDERR, "usage: php gt_php.php <repo> <out.json>\n");
    exit(2);
}
$repo = rtrim(str_replace('\\', '/', realpath($repoArg)), '/');

$all = [];   // repo-relative path => true, every non-vendor file
walk($repo, '', $all);
$php = array_values(array_filter(array_keys($all), fn ($f) => str_ends_with(strtolower($f), '.php')));
sort($php);

// Autoload maps from every composer.json (vendor directories are never walked).
$psr4 = [];
$psr0 = [];
foreach (array_keys($all) as $f) {
    if (basename($f) !== 'composer.json') {
        continue;
    }
    $json = json_decode(file_get_contents("$repo/$f"), true);
    $base = dirname($f) === '.' ? '' : dirname($f);
    foreach (['autoload', 'autoload-dev'] as $section) {
        foreach (['psr-4' => &$psr4, 'psr-0' => &$psr0] as $kind => &$map) {
            foreach (($json[$section][$kind] ?? []) as $prefix => $dirs) {
                foreach ((array) $dirs as $d) {
                    $dir = normalize(($base === '' ? '' : "$base/") . $d);
                    if ($dir !== null) {
                        $map[] = [$prefix, $dir];
                    }
                }
            }
        }
        unset($map);
    }
}
$byLen = fn ($a, $b) => strlen($b[0]) <=> strlen($a[0]);
usort($psr4, $byLen);
usort($psr0, $byLen);

// Pass 1: facts per file. Pass 2: resolve.
$facts = [];
$classes = [];
$functions = [];
$consts = [];
foreach ($php as $f) {
    $facts[$f] = analyze(file_get_contents("$repo/$f"));
    foreach ($facts[$f]['classes'] as $c) {
        $classes[strtolower($c)][$f] = true;
    }
    foreach ($facts[$f]['functions'] as $c) {
        $functions[strtolower($c)][$f] = true;
    }
    foreach ($facts[$f]['consts'] as $c) {
        $consts[constKey($c)][$f] = true;
    }
}

$universe = array_flip($php);
$edges = [];
foreach ($facts as $from => $fx) {
    $targets = [];
    foreach ($fx['refs'] as $fqn => $_) {
        foreach (classFiles($fqn) as $t) {
            $targets[$t] = true;
        }
    }
    foreach ($fx['use_functions'] as $fqn => $_) {
        $targets += $functions[strtolower($fqn)] ?? [];
    }
    foreach ($fx['use_consts'] as $fqn => $_) {
        $targets += $consts[constKey($fqn)] ?? [];
    }
    foreach ($fx['includes'] as $inc) {
        $p = normalize((dirname($from) === '.' ? '' : dirname($from) . '/') . $inc);
        if ($p !== null && isset($all[$p])) {
            $targets[$p] = true;
        }
    }
    foreach (array_keys($targets) as $to) {
        if ($to !== $from && isset($universe[$to])) {
            $edges["$from\0$to"] = [$from, $to];
        }
    }
}
ksort($edges);
file_put_contents($outPath, json_encode([
    'tool' => 'php ' . PHP_VERSION . ' PhpToken::tokenize name resolution + composer psr-4/psr-0 + declared-symbol table',
    'universe' => $php,
    'edges' => array_values($edges),
    'autoload' => ['psr-4' => $psr4, 'psr-0' => $psr0],
], JSON_UNESCAPED_SLASHES));
echo 'universe=' . count($php) . ' edges=' . count($edges) . "\n";

function classFiles(string $fqn): array
{
    global $psr4, $psr0, $all, $classes;
    foreach ($psr4 as [$prefix, $dir]) {
        $prefix = rtrim($prefix, '\\') === '' ? '' : rtrim($prefix, '\\') . '\\';
        if ($prefix === '' || str_starts_with($fqn, $prefix)) {
            $p = ($dir === '' ? '' : "$dir/") . str_replace('\\', '/', substr($fqn, strlen($prefix))) . '.php';
            if (isset($all[$p])) {
                return [$p];
            }
        }
    }
    foreach ($psr0 as [$prefix, $dir]) {
        if ($prefix === '' || str_starts_with($fqn, $prefix)) {
            $pos = strrpos($fqn, '\\');
            $logical = $pos === false
                ? str_replace('_', '/', $fqn)
                : str_replace('\\', '/', substr($fqn, 0, $pos)) . '/' . str_replace('_', '/', substr($fqn, $pos + 1));
            $p = ($dir === '' ? '' : "$dir/") . $logical . '.php';
            if (isset($all[$p])) {
                return [$p];
            }
        }
    }
    return array_keys($classes[strtolower($fqn)] ?? []);
}

function constKey(string $fqn): string
{
    $pos = strrpos($fqn, '\\');
    return $pos === false ? $fqn : strtolower(substr($fqn, 0, $pos)) . '\\' . substr($fqn, $pos + 1);
}

function walk(string $root, string $rel, array &$out): void
{
    foreach (scandir($rel === '' ? $root : "$root/$rel") as $n) {
        if ($n === '.' || $n === '..') {
            continue;
        }
        $p = $rel === '' ? $n : "$rel/$n";
        if (is_dir("$root/$p")) {
            if ($n[0] !== '.' && $n !== 'vendor' && $n !== 'node_modules') {
                walk($root, $p, $out);
            }
        } else {
            $out[$p] = true;
        }
    }
}

function normalize(string $path): ?string
{
    $out = [];
    foreach (explode('/', str_replace('\\', '/', $path)) as $seg) {
        if ($seg === '' || $seg === '.') {
            continue;
        }
        if ($seg === '..') {
            if (!$out) {
                return null;
            }
            array_pop($out);
        } else {
            $out[] = $seg;
        }
    }
    return implode('/', $out);
}

function isName($t): bool
{
    return $t instanceof PhpToken && in_array($t->id, NAME_IDS, true);
}

/** Facts of one file: declarations, class references (fully qualified), use function/const, includes. */
function analyze(string $code): array
{
    $toks = [];
    foreach (PhpToken::tokenize($code) as $t) {
        if (in_array($t->id, [T_WHITESPACE, T_COMMENT, T_DOC_COMMENT, T_OPEN_TAG, T_INLINE_HTML], true)) {
            continue;
        }
        if ($t->id === T_CLOSE_TAG || $t->id === T_OPEN_TAG_WITH_ECHO) {
            $t = new PhpToken(ord(';'), ';');
        }
        $toks[] = $t;
    }
    $n = count($toks);
    $st = [
        'ns' => '', 'region' => 0, 'aliases' => [], 'classes' => [], 'functions' => [], 'consts' => [],
        'refs' => [], 'use_functions' => [], 'use_consts' => [], 'includes' => [],
    ];
    $regions = 0;
    $stack = [];          // 'ns' | 'class' | 'other'
    $pendingNs = false;
    $pendingClass = false;
    $heritage = false;
    $ref = function (string $name, int $id) use (&$st): void {
        if (in_array(strtolower(ltrim($name, '\\')), NOT_CLASSES, true)) {
            return;
        }
        $st['refs'][qualify($name, $id, $st)] = true;
    };
    $topLevel = function () use (&$stack): bool {
        return !array_diff($stack, ['ns']);
    };
    $inClassBody = function () use (&$stack): bool {
        return $stack && end($stack) === 'class';
    };

    for ($i = 0; $i < $n; $i++) {
        $t = $toks[$i];
        $prev = $i > 0 ? $toks[$i - 1] : null;
        $next = $toks[$i + 1] ?? null;

        if ($t->id === T_NAMESPACE && $next && ($next->text === '{' || isName($next))) {
            $name = '';
            $j = $i + 1;
            if (isName($toks[$j])) {
                $name = ltrim($toks[$j]->text, '\\');
                $j++;
            }
            $st['ns'] = $name;
            $st['region'] = ++$regions;
            if (($toks[$j]->text ?? '') === '{') {
                $pendingNs = true;
            }
            $i = $j - 1;
            continue;
        }
        if ($t->text === '{' || $t->id === T_CURLY_OPEN || $t->id === T_DOLLAR_OPEN_CURLY_BRACES) {
            $kind = $t->text === '{' && $pendingNs ? 'ns' : ($t->text === '{' && $pendingClass ? 'class' : 'other');
            $stack[] = $kind;
            $pendingNs = $pendingClass = $heritage = false;
            continue;
        }
        if ($t->text === '}') {
            if (array_pop($stack) === 'ns') {
                $st['ns'] = '';
                $st['region'] = ++$regions;
            }
            continue;
        }
        if ($t->id === T_USE) {
            if ($topLevel()) {
                $i = useImport($toks, $i, $st, $ref);
            } elseif ($inClassBody()) {
                for ($j = $i + 1; $j < $n && $toks[$j]->text !== ';' && $toks[$j]->text !== '{'; $j++) {
                    if (isName($toks[$j])) {
                        $ref($toks[$j]->text, $toks[$j]->id);
                    }
                }
                $i = $j - 1;
            }
            continue;
        }
        if (in_array($t->id, [T_CLASS, T_INTERFACE, T_TRAIT, T_ENUM], true)) {
            if ($prev && $prev->id === T_DOUBLE_COLON) {
                continue; // X::class
            }
            $pendingClass = true;
            if (!($prev && $prev->id === T_NEW) && $next && $next->id === T_STRING) {
                $st['classes'][] = qualifyDecl($next->text, $st['ns']);
                $i++;
            }
            continue;
        }
        if ($t->id === T_EXTENDS || $t->id === T_IMPLEMENTS) {
            $heritage = true;
            continue;
        }
        if ($t->id === T_FUNCTION || $t->id === T_FN) {
            $j = $i + 1;
            if (($toks[$j]->text ?? '') === '&') {
                $j++;
            }
            if (isset($toks[$j]) && $toks[$j]->id === T_STRING && ($toks[$j + 1]->text ?? '') === '(') {
                if (!$inClassBody()) {
                    $st['functions'][] = qualifyDecl($toks[$j]->text, $st['ns']);
                }
                $j++;
            }
            if (($toks[$j]->text ?? '') === '(') {
                $j = signature($toks, $j, $ref);
            }
            $i = $j - 1;
            continue;
        }
        if ($t->id === T_CONST) {
            if ($inClassBody()) {
                if (isName($next) && ($toks[$i + 2] ?? null) && $toks[$i + 2]->id === T_STRING) {
                    $ref($next->text, $next->id); // typed class constant (PHP 8.3)
                }
                continue;
            }
            $depth = 0;
            $expectName = true;
            for ($j = $i + 1; $j < $n && !($depth === 0 && $toks[$j]->text === ';'); $j++) {
                $x = $toks[$j]->text;
                if ($x === '(' || $x === '[') {
                    $depth++;
                } elseif ($x === ')' || $x === ']') {
                    $depth--;
                } elseif ($depth === 0 && $x === ',') {
                    $expectName = true;
                } elseif ($expectName && $toks[$j]->id === T_STRING) {
                    $st['consts'][] = qualifyDecl($x, $st['ns']);
                    $expectName = false;
                }
            }
            continue;
        }
        if ($inClassBody() && in_array($t->id, MODIFIERS, true)) {
            $j = $i;
            while ($j < $n && in_array($toks[$j]->id, MODIFIERS, true)) {
                $j++;
            }
            $types = [];
            while ($j < $n && (isName($toks[$j]) || in_array($toks[$j]->text, ['?', '|', '&', '(', ')'], true))) {
                if (isName($toks[$j])) {
                    $types[] = $toks[$j];
                }
                $j++;
            }
            if ($j < $n && $toks[$j]->id === T_VARIABLE) {
                foreach ($types as $ty) {
                    $ref($ty->text, $ty->id);
                }
            }
            continue;
        }
        if ($t->id === T_CATCH && ($next->text ?? '') === '(') {
            for ($j = $i + 2; $j < $n && $toks[$j]->id !== T_VARIABLE && $toks[$j]->text !== ')'; $j++) {
                if (isName($toks[$j])) {
                    $ref($toks[$j]->text, $toks[$j]->id);
                }
            }
            continue;
        }
        if ($t->id === T_ATTRIBUTE) {
            $depth = 0;
            $expect = true;
            for ($j = $i + 1; $j < $n; $j++) {
                $x = $toks[$j]->text;
                if ($depth === 0 && $x === ']') {
                    break;
                }
                if ($x === '(' || $x === '[') {
                    $depth++;
                } elseif ($x === ')' || $x === ']') {
                    $depth--;
                } elseif ($depth === 0 && $x === ',') {
                    $expect = true;
                } elseif ($depth === 0 && $expect && isName($toks[$j])) {
                    $ref($x, $toks[$j]->id);
                    $expect = false;
                }
            }
            continue; // argument tokens are revisited by the generic rules
        }
        if (in_array($t->id, [T_REQUIRE, T_REQUIRE_ONCE, T_INCLUDE, T_INCLUDE_ONCE], true)) {
            if (($p = includePath($toks, $i)) !== null) {
                $st['includes'][] = $p;
            }
            continue;
        }
        if (isName($t)) {
            $byPrev = $prev && (in_array($prev->id, [T_NEW, T_EXTENDS, T_IMPLEMENTS, T_INSTANCEOF, T_INSTEADOF], true)
                || ($heritage && $prev->text === ','));
            if ($byPrev || ($next && $next->id === T_DOUBLE_COLON)) {
                $ref($t->text, $t->id);
            }
        }
    }
    unset($st['ns'], $st['region'], $st['aliases']);
    return $st;
}

function qualifyDecl(string $name, string $ns): string
{
    return $ns === '' ? $name : "$ns\\$name";
}

/** Fully qualified name of a class reference, by PHP's rules for class names. */
function qualify(string $name, int $id, array $st): string
{
    if ($id === T_NAME_FULLY_QUALIFIED) {
        return substr($name, 1);
    }
    if ($id === T_NAME_RELATIVE) {
        return qualifyDecl(substr($name, strlen('namespace\\')), $st['ns']);
    }
    $parts = explode('\\', $name, 2);
    $alias = $st['aliases'][$st['region']][strtolower($parts[0])] ?? null;
    if ($alias !== null) {
        return isset($parts[1]) ? "$alias\\$parts[1]" : $alias;
    }
    return qualifyDecl($name, $st['ns']);
}

/** Parses a top-level `use` import starting at $i; returns the index of its `;`. */
function useImport(array $toks, int $i, array &$st, callable $ref): int
{
    $n = count($toks);
    $j = $i + 1;
    $kind = 'class';
    if ($toks[$j]->id === T_FUNCTION || $toks[$j]->id === T_CONST) {
        $kind = $toks[$j]->id === T_FUNCTION ? 'function' : 'const';
        $j++;
    }
    $add = function (string $k, string $fqn, ?string $alias) use (&$st, $ref): void {
        $fqn = ltrim($fqn, '\\');
        if ($k === 'class') {
            $alias ??= substr(strrchr('\\' . $fqn, '\\'), 1);
            $st['aliases'][$st['region']][strtolower($alias)] = $fqn;
            $st['refs'][$fqn] = true;
        } elseif ($k === 'function') {
            $st['use_functions'][$fqn] = true;
        } else {
            $st['use_consts'][$fqn] = true;
        }
    };
    while ($j < $n && $toks[$j]->text !== ';') {
        $prefix = '';
        while ($j < $n && (isName($toks[$j]) || $toks[$j]->id === T_NS_SEPARATOR)) {
            $prefix .= $toks[$j]->text;
            $j++;
        }
        if (($toks[$j]->text ?? '') === '{') {
            $j++;
            while ($j < $n && $toks[$j]->text !== '}') {
                $k = $kind;
                if ($toks[$j]->id === T_FUNCTION || $toks[$j]->id === T_CONST) {
                    $k = $toks[$j]->id === T_FUNCTION ? 'function' : 'const';
                    $j++;
                }
                $name = '';
                while ($j < $n && (isName($toks[$j]) || $toks[$j]->id === T_NS_SEPARATOR)) {
                    $name .= $toks[$j]->text;
                    $j++;
                }
                $alias = null;
                if ($toks[$j]->id === T_AS) {
                    $alias = $toks[$j + 1]->text;
                    $j += 2;
                }
                if ($name !== '') {
                    $add($k, $prefix . $name, $alias);
                }
                if ($toks[$j]->text === ',') {
                    $j++;
                }
            }
            $j++; // '}'
        } else {
            $alias = null;
            if (($toks[$j]->id ?? null) === T_AS) {
                $alias = $toks[$j + 1]->text;
                $j += 2;
            }
            if ($prefix !== '') {
                $add($kind, $prefix, $alias);
            }
        }
        if (($toks[$j]->text ?? '') === ',') {
            $j++;
        }
    }
    return $j;
}

/** Parameter types, closure `use (...)` and the return type of a signature whose `(` is at $j. */
function signature(array $toks, int $j, callable $ref): int
{
    $n = count($toks);
    $depth = 0;
    $param = [];
    // Per parameter: attribute names, the type (up to the variable), and in
    // attribute arguments and default values the names before `::` or after `new`.
    $flush = function () use (&$param, $ref): void {
        $attr = 0;          // bracket depth inside `#[...]`
        $expect = false;    // an attribute name may follow
        $typeDone = false;
        foreach ($param as $k => $t) {
            $prev = $param[$k - 1] ?? null;
            $next = $param[$k + 1] ?? null;
            if (isName($t) && (($prev && $prev->id === T_NEW) || ($next && $next->id === T_DOUBLE_COLON))) {
                $ref($t->text, $t->id);
                $expect = false;
                continue;
            }
            if ($t->id === T_ATTRIBUTE) {
                $attr = 1;
                $expect = true;
                continue;
            }
            if ($attr > 0) {
                if ($t->text === '[' || $t->text === '(') {
                    $attr++;
                } elseif ($t->text === ']' || $t->text === ')') {
                    $attr--;
                } elseif ($attr === 1 && $t->text === ',') {
                    $expect = true;
                } elseif ($attr === 1 && $expect && isName($t)) {
                    $ref($t->text, $t->id);
                    $expect = false;
                }
                continue;
            }
            if ($t->id === T_VARIABLE || $t->id === T_ELLIPSIS || $t->text === '=') {
                $typeDone = true;
            }
            if (!$typeDone && isName($t)) {
                $ref($t->text, $t->id);
            }
        }
        $param = [];
    };
    for (; $j < $n; $j++) {
        $x = $toks[$j]->text;
        if ($x === '(') {
            if ($depth++ === 0) {
                continue;
            }
        } elseif ($x === ')') {
            if (--$depth === 0) {
                $flush();
                $j++;
                break;
            }
        } elseif ($depth === 1 && $x === ',') {
            $flush();
            continue;
        }
        $param[] = $toks[$j];
    }
    if (($toks[$j]->id ?? null) === T_USE) {
        for ($d = 0; $j < $n; $j++) {
            if ($toks[$j]->text === '(') {
                $d++;
            } elseif ($toks[$j]->text === ')' && --$d === 0) {
                $j++;
                break;
            }
        }
    }
    if (($toks[$j]->text ?? '') === ':') {
        for ($j++; $j < $n && !in_array($toks[$j]->text, ['{', ';'], true) && $toks[$j]->id !== T_DOUBLE_ARROW; $j++) {
            if (isName($toks[$j])) {
                $ref($toks[$j]->text, $toks[$j]->id);
            }
        }
    }
    return $j;
}

/** Static include path relative to the including file's directory, or null. */
function includePath(array $toks, int $i): ?string
{
    $expr = [];
    $depth = 0;
    for ($j = $i + 1; $j < count($toks); $j++) {
        $x = $toks[$j]->text;
        if ($x === '(' ) {
            $depth++;
        } elseif ($x === ')') {
            if (--$depth < 0) {
                break;
            }
        } elseif ($depth === 0 && in_array($x, [';', ',', ']'], true)) {
            break;
        }
        $expr[] = $toks[$j];
    }
    while (count($expr) >= 2 && $expr[0]->text === '(' && end($expr)->text === ')') {
        $expr = array_slice($expr, 1, -1);
    }
    $lit = function ($t): ?string {
        if (!$t || $t->id !== T_CONSTANT_ENCAPSED_STRING) {
            return null;
        }
        $q = $t->text[0];
        $s = substr($t->text, 1, -1);
        return $q === "'" ? str_replace(["\\'", '\\\\'], ["'", '\\'], $s) : stripcslashes($s);
    };
    $texts = array_map(fn ($t) => $t->id === T_NAME_FULLY_QUALIFIED ? ltrim($t->text, '\\') : $t->text, $expr);
    $abs = fn (string $p) => $p === '' || $p[0] === '/' || $p[0] === '\\' || preg_match('#^[A-Za-z]:|://#', $p);
    if (count($expr) === 1 && ($p = $lit($expr[0])) !== null) {
        return $abs($p) ? null : $p;
    }
    // <base> . 'literal'
    $levels = null;
    $rest = null;
    if (count($expr) === 3 && $expr[0]->id === T_DIR && $texts[1] === '.') {
        [$levels, $rest] = [0, $expr[2]];
    } elseif (count($expr) >= 6 && strtolower($texts[0]) === 'dirname' && $texts[1] === '(') {
        $close = array_search(')', $texts, true);
        if ($close !== false && ($texts[$close + 1] ?? null) === '.' && count($expr) === $close + 3) {
            $inner = array_slice($expr, 2, $close - 2);
            if (count($inner) === 1 && $inner[0]->id === T_FILE) {
                $levels = 0;
            } elseif (count($inner) === 1 && $inner[0]->id === T_DIR) {
                $levels = 1;
            } elseif (count($inner) === 3 && $inner[0]->id === T_DIR && $inner[1]->text === ',' && $inner[2]->id === T_LNUMBER) {
                $levels = (int) $inner[2]->text;
            }
            $rest = $expr[$close + 2];
        }
    }
    if ($levels === null || ($p = $lit($rest)) === null || $p === '' || $p[0] !== '/') {
        return null;
    }
    return str_repeat('../', $levels) . substr($p, 1);
}
