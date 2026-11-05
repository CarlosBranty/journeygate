"""Unit tests for metrics.py (run: python -I -m unittest discover -s scripts)."""

import unittest

from metrics import bounded_dependents, edge_scores, language_decision, set_scores


class MetricsTest(unittest.TestCase):
    def test_edge_scores_restrict_to_universe_and_drop_self_edges(self):
        truth = [["a", "b"], ["b", "c"]]
        predicted = [["a", "b"], ["a", "x"], ["c", "c"], ["c", "a"]]
        s = edge_scores(truth, predicted, universe={"a", "b", "c"})
        self.assertEqual((s["tp"], s["fp"], s["fn"]), (1, 1, 1))
        self.assertAlmostEqual(s["precision"], 0.5)
        self.assertAlmostEqual(s["recall"], 0.5)
        self.assertAlmostEqual(s["f1"], 0.5)

    def test_edge_scores_empty_prediction_scores_zero(self):
        s = edge_scores([["a", "b"]], [], universe={"a", "b"})
        self.assertEqual((s["precision"], s["recall"], s["f1"]), (0.0, 0.0, 0.0))

    def test_bounded_dependents_reverse_bfs(self):
        edges = [["b", "a"], ["c", "b"], ["d", "c"], ["z", "d"], ["e", "a"]]
        self.assertEqual(bounded_dependents(edges, ["a"], 3), {"b", "c", "d", "e"})
        self.assertEqual(bounded_dependents(edges, ["a"], 1), {"b", "e"})
        self.assertEqual(bounded_dependents(edges, ["a", "b"], 1), {"c", "e"})

    def test_set_scores_none_when_undefined(self):
        self.assertEqual(set_scores(set(), set()), {"precision": None, "recall": None})
        self.assertEqual(set_scores({"a", "b"}, {"a"}), {"precision": 1.0, "recall": 0.5})

    def test_language_decision_prefers_codegraph_when_either_input_is_lower(self):
        # (gi_f1, cg_f1, gi_recall, cg_recall)
        self.assertEqual(language_decision("csharp", 0.9, 0.5, 0.95, 0.80), "guard_impact_better_for_csharp")
        self.assertEqual(language_decision("php", 0.4, 0.5, 0.95, 0.80), "codegraph_better_for_php")
        self.assertEqual(language_decision("php", 0.9, 0.5, 0.79, 0.80), "codegraph_better_for_php")
        # Ties are not "lower".
        self.assertEqual(language_decision("csharp", 0.5, 0.5, 0.8, 0.8), "guard_impact_better_for_csharp")


if __name__ == "__main__":
    unittest.main()
