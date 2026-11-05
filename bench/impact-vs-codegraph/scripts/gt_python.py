"""Ground truth for Python repos.

Imports are extracted with CPython's own parser (`ast`, every `Import` /
`ImportFrom` node anywhere in the module, including function bodies and
`TYPE_CHECKING` blocks) and resolved with CPython's import machinery
(`importlib.machinery.PathFinder` walked package by package, and
`importlib.util.resolve_name` for relative imports), never executing code.

Search path of a file (mirrors how the file is actually imported/run):
  1. its *basedir*: the first ancestor directory that is not a package (no
     `__init__.py`), i.e. pytest's `rootdir`-prepend rule / script directory;
  2. the repository root;
  3. `<root>/src` when it exists.

Edge semantics: `import a.b.c` -> the module `a.b.c`; `from a.b import x` ->
the module `a.b`, plus `a.b.x` when `x` is a submodule. Implicit execution of
parent packages' `__init__.py` is not counted as an edge.

usage: python -I gt_python.py <repo> <out.json>
"""

import ast
import importlib.machinery
import importlib.util
import json
import os
import subprocess
import sys


def tracked_files(root):
    out = subprocess.run(["git", "-C", root, "ls-files"], capture_output=True, text=True, check=True)
    return [line for line in out.stdout.splitlines() if line]


def basedir_of(root, rel):
    d = os.path.dirname(os.path.join(root, rel))
    while os.path.isfile(os.path.join(d, "__init__.py")) and os.path.normpath(d) != os.path.normpath(root):
        d = os.path.dirname(d)
    return d


def module_name(basedir, abs_path):
    rel = os.path.relpath(abs_path, basedir).replace(os.sep, "/")
    rel = rel[:-3] if rel.endswith(".py") else rel
    parts = rel.split("/")
    if parts[-1] == "__init__":
        parts = parts[:-1]
    return ".".join(parts)


def find_module(fullname, search):
    """Origin file of `fullname` under `search`, or None (namespace/missing)."""
    parts = fullname.split(".")
    path = search
    spec = None
    for i in range(len(parts)):
        name = ".".join(parts[: i + 1])
        spec = importlib.machinery.PathFinder.find_spec(name, path)
        if spec is None:
            return None
        if i < len(parts) - 1:
            if spec.submodule_search_locations is None:
                return None
            path = list(spec.submodule_search_locations)
    if spec is None or spec.origin in (None, "namespace") or not spec.has_location:
        return None
    return spec.origin


def main():
    root, out = os.path.abspath(sys.argv[1]), sys.argv[2]
    universe = [f for f in tracked_files(root) if f.endswith(".py")]
    in_universe = set(universe)
    norm_root = os.path.normcase(os.path.normpath(root))

    def to_rel(origin):
        if origin is None:
            return None
        o = os.path.normcase(os.path.normpath(origin))
        if not o.startswith(norm_root + os.sep):
            return None
        rel = os.path.relpath(origin, root).replace(os.sep, "/")
        return rel if rel in in_universe else None

    edges = []
    stats = {"statements": 0, "parse_errors": 0}
    src = os.path.join(root, "src")
    for rel in universe:
        abs_path = os.path.join(root, rel)
        base = basedir_of(root, rel)
        search = [base, root] + ([src] if os.path.isdir(src) else [])
        me = module_name(base, abs_path)
        is_pkg = rel.endswith("__init__.py")
        package = me if is_pkg else me.rpartition(".")[0]
        try:
            tree = ast.parse(open(abs_path, encoding="utf-8", errors="replace").read(), filename=rel)
        except SyntaxError:
            stats["parse_errors"] += 1
            continue
        targets = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                for alias in node.names:
                    stats["statements"] += 1
                    targets.add(to_rel(find_module(alias.name, search)))
            elif isinstance(node, ast.ImportFrom):
                stats["statements"] += 1
                try:
                    mod = importlib.util.resolve_name("." * node.level + (node.module or ""), package) if node.level else node.module
                except (ImportError, ValueError):
                    continue
                if not mod:
                    continue
                targets.add(to_rel(find_module(mod, search)))
                for alias in node.names:
                    if alias.name != "*":
                        targets.add(to_rel(find_module(f"{mod}.{alias.name}", search)))
        targets.discard(None)
        targets.discard(rel)
        edges.extend([rel, t] for t in sorted(targets))
    tool = f"CPython {sys.version.split()[0]} ast + importlib.machinery.PathFinder"
    json.dump({"tool": tool, "universe": universe, "edges": edges, "stats": stats}, open(out, "w"))
    print(f"universe={len(universe)} edges={len(edges)} {stats}")


if __name__ == "__main__":
    main()
