"""Fixture tests for the C# (Roslyn) and PHP (PhpToken) ground-truth tools.

Each tool runs on a small fixture under ../fixtures/ and must reproduce the
hand-checked universe and edge set in that fixture's expected.json.

usage (from scripts/): python -m unittest test_gt
"""

import json
import os
import subprocess
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
BENCH = os.path.dirname(HERE)
FIXTURES = os.path.join(BENCH, "fixtures")


def run_tool(cmd, fixture):
    with tempfile.TemporaryDirectory() as tmp:
        out = os.path.join(tmp, "gt.json")
        subprocess.run([*cmd, os.path.join(FIXTURES, fixture), out], check=True, capture_output=True, text=True)
        with open(out, encoding="utf-8") as fh:
            return json.load(fh)


def expected(fixture):
    with open(os.path.join(FIXTURES, fixture, "expected.json"), encoding="utf-8") as fh:
        return json.load(fh)


class GroundTruthFixtures(unittest.TestCase):
    def check(self, got, want):
        self.assertEqual(sorted(got["universe"]), sorted(want["universe"]))
        self.assertEqual(sorted(map(tuple, got["edges"])), sorted(map(tuple, want["edges"])))
        self.assertTrue(got["tool"])

    def test_csharp_roslyn(self):
        project = os.path.join(BENCH, "gt_csharp", "gt_csharp.csproj")
        subprocess.run(["dotnet", "build", project, "-c", "Release", "-v", "q", "-nologo"], check=True, capture_output=True)
        dll = os.path.join(BENCH, "gt_csharp", "bin", "Release", "net10.0", "gt_csharp.dll")
        self.check(run_tool(["dotnet", dll], "gt_csharp"), expected("gt_csharp"))

    def test_php_tokenizer(self):
        self.check(run_tool(["php", os.path.join(HERE, "gt_php.php")], "gt_php"), expected("gt_php"))


if __name__ == "__main__":
    unittest.main()
