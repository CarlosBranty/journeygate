"""Deterministic commit sample for the impact-selection metric.

Walks `git log --no-merges` from the pinned HEAD (skipping the shallow
boundary commit, whose diff against a missing parent is meaningless) and keeps
the most recent commits whose changed files, intersected with the ground-truth
universe at HEAD, are non-empty AND have a non-empty ground-truth depth-3
dependent set (recall is undefined otherwise). Up to MAX_COMMITS are kept;
the rule was fixed before any tool output was looked at.

usage: python sample_commits.py <repo> <gt.json> <out-changesets.json>
"""

import json
import os
import subprocess
import sys

from metrics import bounded_dependents

MAX_COMMITS = 50
DEPTH = 3


def git(repo, *args):
    return subprocess.run(["git", "-C", repo, *args], capture_output=True, text=True, check=True).stdout


def main():
    repo, gt_path, out = sys.argv[1:4]
    gt = json.load(open(gt_path))
    universe = set(gt["universe"])
    shallow_file = os.path.join(repo, git(repo, "rev-parse", "--git-path", "shallow").strip())
    shallow = set(open(shallow_file).read().split()) if os.path.isfile(shallow_file) else set()
    sets, considered = [], 0
    for sha in git(repo, "log", "--no-merges", "--format=%H").split():
        if sha in shallow:
            continue
        considered += 1
        files = [f for f in git(repo, "diff-tree", "--no-commit-id", "--name-only", "-r", sha).split("\n") if f]
        changed = sorted(set(files) & universe)
        if not changed or not bounded_dependents(gt["edges"], changed, DEPTH):
            continue
        sets.append({"id": sha, "files": changed})
        if len(sets) == MAX_COMMITS:
            break
    json.dump(sets, open(out, "w"))
    print(f"considered={considered} kept={len(sets)}")


if __name__ == "__main__":
    main()
