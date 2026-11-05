"""Ground truth for Rust workspaces: the module tree plus `use` resolution.

No rust-analyzer is installed on the benchmark machine (the rustup proxy has
no component), so this follows the Rust Reference rules directly:

* Crates and their targets come from `cargo metadata --format-version 1
  --no-deps` (lib/bin/test/bench/example/custom-build roots, real crate names,
  path dependencies and renames). Each target root starts a module tree.
* The tree is built from `mod name;` declarations (file `name.rs` or
  `name/mod.rs` in the module's directory; non-`mod.rs` files own the
  directory named after their stem), following inline `mod name { ... }`
  blocks. Comments, strings and char literals are blanked first. `#[path]`
  attributes are not supported (the benchmark repo uses none; checked).
* Edges: every `mod name;` (parent file -> child file) and every path of every
  `use` tree (groups, `self`, globs, renames) and `extern crate`, resolved
  from the declaring module: `crate::`, `self::`, `super::`, `::extern`,
  2018 uniform paths (a child module of the current module first, then an
  extern crate visible to the target). The target file is the deepest module
  of the path that exists (an item path lands on the file of the module that
  holds the item; the crate root when only the crate itself matches).
  Paths into std / crates.io are dropped.

usage: python -I gt_rust.py <repo> <cargo-metadata.json> <out.json>
"""

import json
import os
import re
import sys


def blank(text):
    """Replace comments, string and char literals with spaces (newlines kept)."""
    out = []
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        if text.startswith("//", i):
            j = text.find("\n", i)
            j = n if j < 0 else j
            out.append(" " * (j - i))
            i = j
        elif text.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if text.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif text.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            out.append(re.sub(r"[^\n]", " ", text[i:j]))
            i = j
        elif (m := re.match(r'b?r(#*)"', text[i:i + 300])) and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")):
            close = '"' + m.group(1)
            j = text.find(close, i + m.end())
            j = n if j < 0 else j + len(close)
            out.append(re.sub(r"[^\n]", " ", text[i:j]))
            i = j
        elif c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            j = min(n, j + 1)
            out.append(re.sub(r"[^\n]", " ", text[i:j]))
            i = j
        elif c == "'":
            m = re.match(r"'(\\(x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)|[^\\'\n])'", text[i:i + 16])
            if m:
                out.append(" " * m.end())
                i += m.end()
            else:  # lifetime / label
                out.append(c)
                i += 1
        else:
            out.append(c)
            i += 1
    return "".join(out)


def split_top(s, sep=","):
    parts, depth, cur = [], 0, []
    for ch in s:
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
        if ch == sep and depth == 0:
            parts.append("".join(cur))
            cur = []
        else:
            cur.append(ch)
    parts.append("".join(cur))
    return [p.strip() for p in parts if p.strip()]


def use_paths(tree, prefix=()):
    """Expand a use tree into segment tuples (`self` in a group = the prefix)."""
    tree = re.sub(r"\s+as\s+(r#)?\w+", "", tree)  # renames do not change the target
    tree = re.sub(r"\s+", "", tree)
    if not tree:
        return []
    brace = tree.find("{")
    if brace >= 0 and tree.endswith("}"):
        head = tree[:brace].rstrip(":")
        base = prefix + tuple(s for s in head.split("::") if s != "") if head else prefix
        if head.startswith("::"):
            base = prefix + ("::",) + tuple(s for s in head[2:].split("::") if s)
        out = []
        for part in split_top(tree[brace + 1:-1]):
            out.extend(use_paths(part, base))
        return out
    segs = tree.split("::")
    if tree.startswith("::"):
        segs = ["::"] + [s for s in segs[1:] if s]
    segs = [s for s in segs if s and s != "*"]
    if segs == ["self"]:
        return [prefix]
    if segs and segs[-1] == "self":
        segs = segs[:-1]
    return [prefix + tuple(segs)]


USE_RE = re.compile(r"\buse\s+([^;]*);")
EXTERN_RE = re.compile(r"\bextern\s+crate\s+(\w+)(?:\s+as\s+(\w+))?\s*;")
MOD_RE = re.compile(r"\bmod\s+(r#)?(\w+)\s*([;{])")
TOKEN_RE = re.compile(r"[{}]|\buse\s+[^;]*;|\bextern\s+crate\s+\w+(?:\s+as\s+\w+)?\s*;|\bmod\s+(?:r#)?\w+\s*[;{]")


class Crate:
    def __init__(self, name, root_file, externs):
        self.name = name
        self.root_file = root_file
        self.externs = externs  # visible extern crate name -> root file
        self.modules = {(): root_file}  # module path -> file
        self.uses = []  # (file, module path, segments)
        self.mod_edges = []


def build_tree(crate, file, mod_path, child_dir, root, seen):
    if file in seen:
        return
    seen.add(file)
    try:
        text = blank(open(os.path.join(root, file), encoding="utf-8", errors="replace").read())
    except OSError:
        return
    depth = 0
    stack = []  # (name, depth at open)
    for m in TOKEN_RE.finditer(text):
        tok = m.group(0)
        cur_path = mod_path + tuple(s for s, _ in stack)
        cur_dir = "/".join([child_dir] + [s for s, _ in stack]) if child_dir else "/".join(s for s, _ in stack)
        if tok == "{":
            depth += 1
        elif tok == "}":
            depth -= 1
            if stack and stack[-1][1] == depth:
                stack.pop()
        elif tok.startswith("use"):
            for segs in use_paths(USE_RE.match(tok).group(1)):
                crate.uses.append((file, cur_path, segs))
        elif tok.startswith("extern"):
            em = EXTERN_RE.match(tok)
            crate.uses.append((file, cur_path, ("::", em.group(1))))
            if em.group(2) and em.group(1) in crate.externs:
                crate.externs[em.group(2)] = crate.externs[em.group(1)]
        else:
            mm = MOD_RE.match(tok)
            name, kind = mm.group(2), mm.group(3)
            if kind == "{":
                stack.append((name, depth))
                crate.modules.setdefault(cur_path + (name,), file)
                depth += 1
                continue
            base = f"{cur_dir}/{name}" if cur_dir else name
            for cand in (f"{base}.rs", f"{base}/mod.rs"):
                if os.path.isfile(os.path.join(root, cand)):
                    crate.modules[cur_path + (name,)] = cand
                    crate.mod_edges.append((file, cand))
                    own_dir = os.path.dirname(cand) if cand.endswith("/mod.rs") else cand[:-3]
                    build_tree(crate, cand, cur_path + (name,), own_dir, root, seen)
                    break


def resolve(crate, mod_path, segs, crates_by_root):
    if not segs:
        return None
    first, rest = segs[0], list(segs[1:])
    target_crate, abs_path = crate, None
    if first == "crate":
        abs_path = rest
    elif first == "self":
        abs_path = list(mod_path) + rest
    elif first == "super":
        abs_path = list(mod_path)[:-1]
        while rest and rest[0] == "super":
            abs_path = abs_path[:-1]
            rest = rest[1:]
        abs_path += rest
    elif first == "::":
        if not rest or rest[0] not in crate.externs:
            return None
        target_crate = crates_by_root[crate.externs[rest[0]]]
        abs_path = rest[1:]
    elif tuple(mod_path) + (first,) in crate.modules:
        abs_path = list(mod_path) + [first] + rest
    elif first in crate.externs:
        target_crate = crates_by_root[crate.externs[first]]
        abs_path = rest
    else:
        return None
    for k in range(len(abs_path), -1, -1):
        f = target_crate.modules.get(tuple(abs_path[:k]))
        if f:
            return f
    return target_crate.root_file


def main():
    root, meta_path, out = os.path.abspath(sys.argv[1]), sys.argv[2], sys.argv[3]
    meta = json.load(open(meta_path))
    rel = lambda p: os.path.relpath(p, root).replace(os.sep, "/")
    libs = {}  # package name -> (lib crate name, root file)
    for p in meta["packages"]:
        for t in p["targets"]:
            if "lib" in t["kind"] or "rlib" in t["kind"] or "proc-macro" in t["kind"]:
                libs[p["name"]] = (t["name"].replace("-", "_"), rel(t["src_path"]))
    crates = []
    for p in meta["packages"]:
        dep_externs = {}
        for d in p["dependencies"]:
            if d.get("path") and d["name"] in libs:
                name = (d.get("rename") or libs[d["name"]][0]).replace("-", "_")
                dep_externs[name] = libs[d["name"]][1]
        for t in p["targets"]:
            root_file = rel(t["src_path"])
            externs = dict(dep_externs)
            is_lib = root_file == libs.get(p["name"], (None, None))[1]
            if not is_lib and p["name"] in libs:
                externs[libs[p["name"]][0]] = libs[p["name"]][1]
            c = Crate(t["name"], root_file, externs)
            build_tree(c, root_file, (), os.path.dirname(root_file), root, set())
            crates.append(c)
    # A lib crate is the authority for its own modules; other targets that
    # reference it by root file need the lib's module table.
    crates_by_root = {}
    for c in crates:
        crates_by_root.setdefault(c.root_file, c)
    edges, universe = set(), set()
    for c in crates:
        universe.update(c.modules.values())
        for a, b in c.mod_edges:
            edges.add((a, b))
        for file, mod_path, segs in c.uses:
            t = resolve(c, mod_path, segs, crates_by_root)
            if t and t != file:
                edges.add((file, t))
    json.dump({
        "tool": "cargo metadata --no-deps + Rust Reference module-tree/use resolver (gt_rust.py)",
        "universe": sorted(universe),
        "edges": sorted([list(e) for e in edges]),
        "stats": {"targets": len(crates)},
    }, open(out, "w"))
    print(f"universe={len(universe)} edges={len(edges)} targets={len(crates)}")


if __name__ == "__main__":
    main()
