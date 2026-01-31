#!/usr/bin/env python3
"""
Latency Sweep Analyzer

Processes collected metrics and produces decision recommendations
based on p99 latency and stability (coefficient of variation).

Decision Rule:
    SCORE = 0.7 * normalized_p99 + 0.3 * normalized_cv
    Lower score = better

Hard Constraints (disqualifying):
    - p99 > 50ms
    - reconnects > 1/hour
    - p999/p99 ratio > 3.0
"""

import argparse
import json
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Dict, List, Optional, Tuple

try:
    import numpy as np
    from scipy import stats
except ImportError:
    print("Installing numpy and scipy...")
    import subprocess
    subprocess.check_call([sys.executable, "-m", "pip", "install", "numpy", "scipy"])
    import numpy as np
    from scipy import stats


@dataclass
class ConfigMetrics:
    region: str
    instance_family: str
    p50_samples: List[int] = field(default_factory=list)
    p99_samples: List[int] = field(default_factory=list)
    p999_samples: List[int] = field(default_factory=list)
    cv_samples: List[float] = field(default_factory=list)
    reconnects: int = 0
    errors: int = 0
    message_count: int = 0

    @property
    def key(self) -> Tuple[str, str]:
        return (self.region, self.instance_family)


def load_results(results_file: Path) -> Dict[Tuple[str, str], ConfigMetrics]:
    """Load and aggregate results by (region, instance_family)."""
    metrics: Dict[Tuple[str, str], ConfigMetrics] = {}

    with open(results_file) as f:
        for line_num, line in enumerate(f, 1):
            try:
                record = json.loads(line.strip())
            except json.JSONDecodeError as e:
                print(f"Warning: Skipping malformed line {line_num}: {e}")
                continue

            key = (record["region"], record["instance_family"])

            if key not in metrics:
                metrics[key] = ConfigMetrics(
                    region=record["region"],
                    instance_family=record["instance_family"]
                )

            m = metrics[key]
            agg = record["metrics"].get("aggregate", {})

            if agg.get("count", 0) > 0:
                m.p50_samples.append(agg.get("p50_us", 0))
                m.p99_samples.append(agg.get("p99_us", 0))
                m.p999_samples.append(agg.get("p999_us", 0))
                m.cv_samples.append(agg.get("cv", 0))

            counters = record["metrics"].get("counters", {})
            m.reconnects = max(m.reconnects, counters.get("reconnects", 0))
            m.errors = max(m.errors, counters.get("errors", 0))
            m.message_count = max(m.message_count, counters.get("messages", 0))

    return metrics


def compute_score(m: ConfigMetrics, all_metrics: List[ConfigMetrics]) -> Tuple[float, dict]:
    """
    Compute weighted score for a configuration.

    SCORE = 0.7 * normalized_p99 + 0.3 * normalized_cv

    Lower is better.
    """
    if not m.p99_samples:
        return float('inf'), {"error": "no samples"}

    # Get all p99 and CV values for normalization
    all_p99 = [np.median(x.p99_samples) for x in all_metrics if x.p99_samples]
    all_cv = [np.median(x.cv_samples) for x in all_metrics if x.cv_samples]

    if not all_p99 or not all_cv:
        return float('inf'), {"error": "insufficient data"}

    min_p99, max_p99 = min(all_p99), max(all_p99)
    max_cv = max(all_cv)

    # This config's values
    p99 = np.median(m.p99_samples)
    cv = np.median(m.cv_samples)

    # Normalize (0 = best, 1 = worst)
    if max_p99 > min_p99:
        norm_p99 = (p99 - min_p99) / (max_p99 - min_p99)
    else:
        norm_p99 = 0

    norm_cv = cv / max_cv if max_cv > 0 else 0

    # Weighted score (lower is better)
    score = 0.7 * norm_p99 + 0.3 * norm_cv

    details = {
        "p99_median_us": int(p99),
        "p99_p10": int(np.percentile(m.p99_samples, 10)),
        "p99_p90": int(np.percentile(m.p99_samples, 90)),
        "p50_median_us": int(np.median(m.p50_samples)) if m.p50_samples else 0,
        "cv_median": round(cv, 4),
        "p999_median_us": int(np.median(m.p999_samples)) if m.p999_samples else 0,
        "p999_p99_ratio": round(np.median(m.p999_samples) / p99, 2) if p99 > 0 and m.p999_samples else 0,
        "normalized_p99": round(norm_p99, 4),
        "normalized_cv": round(norm_cv, 4),
        "reconnects": m.reconnects,
        "errors": m.errors,
        "message_count": m.message_count,
        "samples": len(m.p99_samples)
    }

    return score, details


def check_hard_constraints(m: ConfigMetrics) -> Tuple[bool, List[str]]:
    """Check if configuration meets hard constraints."""
    violations = []

    if not m.p99_samples:
        return False, ["no data collected"]

    p99 = np.median(m.p99_samples)
    p999 = np.median(m.p999_samples) if m.p999_samples else 0

    # p99 < 50ms (50,000 us)
    if p99 > 50_000:
        violations.append(f"p99 {p99/1000:.1f}ms > 50ms limit")

    # Reconnect rate < 1/hour
    if m.reconnects > 1:
        violations.append(f"Reconnects {m.reconnects} > 1/hour limit")

    # p999/p99 ratio < 3.0
    if p99 > 0 and p999 > 0:
        ratio = p999 / p99
        if ratio > 3.0:
            violations.append(f"p999/p99 ratio {ratio:.2f} > 3.0 limit")

    return len(violations) == 0, violations


def statistical_comparison(m1: ConfigMetrics, m2: ConfigMetrics) -> Optional[dict]:
    """Compare two configurations statistically."""
    if not m1.p99_samples or not m2.p99_samples:
        return None

    if len(m1.p99_samples) < 5 or len(m2.p99_samples) < 5:
        return {"error": "insufficient samples for statistical comparison"}

    # Mann-Whitney U test (non-parametric)
    try:
        stat, p_value = stats.mannwhitneyu(
            m1.p99_samples, m2.p99_samples, alternative='less'
        )
    except ValueError:
        return {"error": "statistical test failed"}

    # Bootstrap confidence interval for difference
    n_bootstrap = 1000
    diffs = []
    rng = np.random.default_rng(42)

    for _ in range(n_bootstrap):
        s1 = rng.choice(m1.p99_samples, size=len(m1.p99_samples), replace=True)
        s2 = rng.choice(m2.p99_samples, size=len(m2.p99_samples), replace=True)
        diffs.append(np.median(s2) - np.median(s1))

    ci_low, ci_high = np.percentile(diffs, [2.5, 97.5])

    return {
        "mann_whitney_p": round(p_value, 4),
        "significant_at_005": p_value < 0.05,
        "significant_at_001": p_value < 0.01,
        "p99_diff_median_us": int(np.median(diffs)),
        "p99_diff_ci_95_us": [int(ci_low), int(ci_high)]
    }


def print_ranking(scored: List[dict]):
    """Print ranked configurations."""
    print("\n" + "=" * 70)
    print("RANKING (lower score = better)")
    print("=" * 70)

    for i, s in enumerate(scored, 1):
        status = "PASS" if s["passes_constraints"] else "FAIL"
        d = s["details"]

        print(f"\n{i}. [{status}] {s['region']} / {s['instance_family']}")
        print(f"   Score: {s['score']:.4f}")
        print(f"   p99: {d['p99_median_us']/1000:.2f}ms "
              f"(range: {d['p99_p10']/1000:.2f}-{d['p99_p90']/1000:.2f}ms)")
        print(f"   p50: {d['p50_median_us']/1000:.2f}ms")
        print(f"   CV (jitter): {d['cv_median']:.4f}")
        print(f"   p999/p99 ratio: {d['p999_p99_ratio']:.2f}")
        print(f"   Messages: {d['message_count']:,}, Reconnects: {d['reconnects']}, Errors: {d['errors']}")
        print(f"   Samples: {d['samples']}")

        if s["violations"]:
            print(f"   VIOLATIONS: {', '.join(s['violations'])}")


def print_comparison(scored: List[dict]):
    """Print statistical comparison between top candidates."""
    passing = [s for s in scored if s["passes_constraints"]]

    if len(passing) < 2:
        return

    print("\n" + "=" * 70)
    print("STATISTICAL COMPARISON (Top 2)")
    print("=" * 70)

    m1 = passing[0]["metrics"]
    m2 = passing[1]["metrics"]
    comparison = statistical_comparison(m1, m2)

    if not comparison or "error" in comparison:
        print(f"Cannot compare: {comparison.get('error', 'unknown error')}")
        return

    print(f"\nComparing: {passing[0]['region']}/{passing[0]['instance_family']} vs "
          f"{passing[1]['region']}/{passing[1]['instance_family']}")
    print(f"Mann-Whitney U p-value: {comparison['mann_whitney_p']}")
    print(f"Significant at p<0.05: {comparison['significant_at_005']}")
    print(f"Significant at p<0.01: {comparison['significant_at_001']}")
    print(f"p99 difference: {comparison['p99_diff_median_us']/1000:.2f}ms "
          f"(95% CI: [{comparison['p99_diff_ci_95_us'][0]/1000:.2f}, "
          f"{comparison['p99_diff_ci_95_us'][1]/1000:.2f}]ms)")


def print_recommendation(scored: List[dict]):
    """Print final recommendation."""
    print("\n" + "=" * 70)
    print("RECOMMENDATION")
    print("=" * 70)

    passing = [s for s in scored if s["passes_constraints"]]

    if not passing:
        print("\nWARNING: No configuration passes all hard constraints!")
        print("Consider:")
        print("  - Expanding the test matrix")
        print("  - Testing at different times of day")
        print("  - Relaxing constraints if justified")
        return

    winner = passing[0]
    d = winner["details"]

    print(f"\nRECOMMENDED: {winner['region']} with {winner['instance_family']}")
    print(f"\nExpected performance:")
    print(f"  p99 latency: {d['p99_median_us']/1000:.2f}ms")
    print(f"  p50 latency: {d['p50_median_us']/1000:.2f}ms")
    print(f"  Stability (CV): {d['cv_median']:.4f}")
    print(f"  Tail ratio (p999/p99): {d['p999_p99_ratio']:.2f}")

    if len(passing) > 1:
        runner_up = passing[1]
        rd = runner_up["details"]
        improvement = (rd["p99_median_us"] - d["p99_median_us"]) / rd["p99_median_us"] * 100

        print(f"\nRunner-up: {runner_up['region']} with {runner_up['instance_family']}")
        print(f"  Winner is {improvement:.1f}% faster on p99")


def export_json(scored: List[dict], output_file: Path):
    """Export results as JSON for further processing."""
    export_data = {
        "timestamp": str(np.datetime64('now')),
        "rankings": [
            {
                "rank": i,
                "region": s["region"],
                "instance_family": s["instance_family"],
                "score": s["score"],
                "passes_constraints": s["passes_constraints"],
                "violations": s["violations"],
                "details": s["details"]
            }
            for i, s in enumerate(scored, 1)
        ],
        "recommendation": None
    }

    passing = [s for s in scored if s["passes_constraints"]]
    if passing:
        export_data["recommendation"] = {
            "region": passing[0]["region"],
            "instance_family": passing[0]["instance_family"],
            "p99_ms": passing[0]["details"]["p99_median_us"] / 1000,
            "cv": passing[0]["details"]["cv_median"]
        }

    with open(output_file, "w") as f:
        json.dump(export_data, f, indent=2)

    print(f"\nExported JSON: {output_file}")


def analyze(results_file: Path, json_output: Optional[Path] = None):
    """Main analysis function."""
    print(f"Analyzing: {results_file}")

    metrics = load_results(results_file)
    all_metrics = list(metrics.values())

    print(f"Loaded {len(all_metrics)} configurations")

    if not all_metrics:
        print("ERROR: No data found in results file")
        sys.exit(1)

    # Score all configurations
    scored = []
    for m in all_metrics:
        passes, violations = check_hard_constraints(m)
        score, details = compute_score(m, all_metrics)

        scored.append({
            "region": m.region,
            "instance_family": m.instance_family,
            "score": score,
            "passes_constraints": passes,
            "violations": violations,
            "details": details,
            "metrics": m
        })

    # Sort by: passing first, then by score (lower is better)
    scored.sort(key=lambda x: (not x["passes_constraints"], x["score"]))

    # Output
    print_ranking(scored)
    print_comparison(scored)
    print_recommendation(scored)

    if json_output:
        export_json(scored, json_output)


def main():
    parser = argparse.ArgumentParser(
        description="Latency Sweep Analyzer",
        formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("results_file", type=Path, help="JSONL results file from run_sweep.py")
    parser.add_argument("--json", type=Path, help="Export results as JSON")

    args = parser.parse_args()

    if not args.results_file.exists():
        print(f"ERROR: Results file not found: {args.results_file}")
        sys.exit(1)

    analyze(args.results_file, args.json)


if __name__ == "__main__":
    main()
