"""Scoring primitives shared by score.py (pure functions, unit-tested)."""

from collections import defaultdict


def edge_scores(truth, predicted, universe):
    """Precision/recall/F1 of directed file->file edges.

    Both edge lists are restricted to edges whose endpoints are in `universe`
    (the ground-truth tool's file set); self-edges are dropped.
    """
    def clean(edges):
        return {(a, b) for a, b in edges if a != b and a in universe and b in universe}

    t, p = clean(truth), clean(predicted)
    tp, fp, fn = len(t & p), len(p - t), len(t - p)
    precision = tp / (tp + fp) if tp + fp else 0.0
    recall = tp / (tp + fn) if tp + fn else 0.0
    f1 = 2 * precision * recall / (precision + recall) if precision + recall else 0.0
    return {"tp": tp, "fp": fp, "fn": fn, "precision": precision, "recall": recall, "f1": f1}


def bounded_dependents(edges, changed, depth):
    """Files reaching any of `changed` within `depth` reverse hops, minus `changed`."""
    importers = defaultdict(set)
    for a, b in edges:
        importers[b].add(a)
    start = set(changed)
    seen, frontier = set(start), set(start)
    for _ in range(depth):
        nxt = {i for f in frontier for i in importers[f]} - seen
        if not nxt:
            break
        seen |= nxt
        frontier = nxt
    return seen - start


def set_scores(truth, predicted):
    """Precision/recall of an affected set; None where the ratio is undefined."""
    tp = len(truth & predicted)
    return {
        "precision": tp / len(predicted) if predicted else None,
        "recall": tp / len(truth) if truth else None,
    }


def language_decision(lang, gi_f1, cg_f1, gi_recall, cg_recall):
    """Per-language rule of the C#/PHP extension (pre-registered 2026-11-01).

    If guard-impact's metric-1 F1 OR its metric-2 recall is strictly lower than
    CodeGraph's for that language, CodeGraph is better for it.
    """
    if gi_f1 < cg_f1 or gi_recall < cg_recall:
        return f"codegraph_better_for_{lang}"
    return f"guard_impact_better_for_{lang}"
