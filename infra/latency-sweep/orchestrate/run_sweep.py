#!/usr/bin/env python3
"""
Latency Sweep Orchestrator

Coordinates multi-region probe deployment, data collection, and analysis.

Usage:
    python run_sweep.py --deploy --phase 1
    python run_sweep.py --collect --warmup 300 --duration 3600
    python run_sweep.py --destroy
"""

import argparse
import json
import subprocess
import sys
import time
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Dict, List, Optional

try:
    import requests
except ImportError:
    print("Installing requests...")
    subprocess.check_call([sys.executable, "-m", "pip", "install", "requests"])
    import requests


@dataclass
class ProbeConfig:
    region: str
    instance_family: str
    instance_ip: str
    instance_id: str
    metrics_port: int = 9090

    @property
    def metrics_url(self) -> str:
        return f"http://{self.instance_ip}:{self.metrics_port}/metrics"

    @property
    def health_url(self) -> str:
        return f"http://{self.instance_ip}:{self.metrics_port}/health"


@dataclass
class SweepConfig:
    experiment_id: str
    warmup_sec: int = 300
    measurement_sec: int = 3600
    poll_interval_sec: int = 60
    output_dir: Path = field(default_factory=lambda: Path("./results"))


def run_terraform(action: str, phase: int = 1, extra_vars: Dict[str, str] = None, auto_approve: bool = True):
    """Run Terraform command in the terraform directory."""
    terraform_dir = Path(__file__).parent.parent / "terraform"

    cmd = ["terraform", action]

    if action == "init":
        pass
    elif action in ("apply", "destroy", "plan"):
        cmd.extend([f"-var=phase={phase}"])
        if extra_vars:
            for k, v in extra_vars.items():
                cmd.extend([f"-var={k}={v}"])
        if auto_approve and action in ("apply", "destroy"):
            cmd.append("-auto-approve")

    print(f"Running: {' '.join(cmd)}")
    result = subprocess.run(cmd, cwd=terraform_dir, check=True)
    return result.returncode == 0


def get_terraform_outputs() -> Dict:
    """Get Terraform outputs as JSON."""
    terraform_dir = Path(__file__).parent.parent / "terraform"
    result = subprocess.run(
        ["terraform", "output", "-json"],
        cwd=terraform_dir,
        capture_output=True,
        text=True,
        check=True
    )
    return json.loads(result.stdout)


def parse_probe_endpoints(outputs: Dict) -> List[ProbeConfig]:
    """Parse Terraform outputs into ProbeConfig objects."""
    probes = []
    endpoints = outputs.get("probe_endpoints", {}).get("value", {})

    for key, data in endpoints.items():
        probes.append(ProbeConfig(
            region=data["region"],
            instance_family=data["instance_family"],
            instance_ip=data["public_ip"],
            instance_id=data["instance_id"],
        ))

    return probes


def wait_for_probes(probes: List[ProbeConfig], timeout_sec: int = 600) -> bool:
    """Wait for all probes to become healthy."""
    print(f"Waiting for {len(probes)} probes to become healthy...")
    start = time.time()

    while time.time() - start < timeout_sec:
        all_healthy = True
        for probe in probes:
            try:
                resp = requests.get(probe.health_url, timeout=5)
                if resp.status_code != 200:
                    all_healthy = False
            except requests.RequestException:
                all_healthy = False

        if all_healthy:
            print("All probes healthy!")
            return True

        elapsed = int(time.time() - start)
        print(f"  [{elapsed}s] Waiting for probes...")
        time.sleep(10)

    print("ERROR: Timeout waiting for probes")
    return False


def collect_metrics(probe: ProbeConfig, timeout: int = 30) -> Optional[dict]:
    """Fetch metrics from a probe instance."""
    try:
        resp = requests.get(probe.metrics_url, timeout=timeout)
        resp.raise_for_status()
        return resp.json()
    except requests.RequestException as e:
        print(f"  Warning: Failed to collect from {probe.region}/{probe.instance_family}: {e}")
        return None


def run_collection(config: SweepConfig, probes: List[ProbeConfig]) -> Path:
    """Execute the metrics collection phase."""
    config.output_dir.mkdir(parents=True, exist_ok=True)
    results_file = config.output_dir / f"{config.experiment_id}_results.jsonl"

    print(f"\n{'='*60}")
    print(f"Starting collection: {config.experiment_id}")
    print(f"Probes: {len(probes)}")
    print(f"Warmup: {config.warmup_sec}s, Measurement: {config.measurement_sec}s")
    print(f"Results: {results_file}")
    print(f"{'='*60}\n")

    # Wait for warmup (probes handle their own warmup internally too)
    print(f"Waiting {config.warmup_sec}s for probe warmup...")
    time.sleep(config.warmup_sec)

    # Collection phase
    start_time = time.time()
    end_time = start_time + config.measurement_sec
    collection_count = 0

    with open(results_file, "a") as f:
        while time.time() < end_time:
            elapsed = int(time.time() - start_time)
            remaining = int(end_time - time.time())
            print(f"\n[{elapsed}s elapsed, {remaining}s remaining] Collecting metrics...")

            for probe in probes:
                metrics = collect_metrics(probe)
                if metrics:
                    record = {
                        "timestamp": datetime.utcnow().isoformat(),
                        "elapsed_sec": elapsed,
                        "region": probe.region,
                        "instance_family": probe.instance_family,
                        "instance_id": probe.instance_id,
                        "metrics": metrics
                    }
                    f.write(json.dumps(record) + "\n")
                    f.flush()
                    collection_count += 1

                    # Print summary
                    agg = metrics.get("aggregate", {})
                    p99 = agg.get("p99_us", 0)
                    cv = agg.get("cv", 0)
                    count = agg.get("count", 0)
                    print(f"  {probe.region}/{probe.instance_family}: "
                          f"p99={p99/1000:.2f}ms, CV={cv:.4f}, samples={count}")

            time.sleep(config.poll_interval_sec)

    print(f"\n{'='*60}")
    print(f"Collection complete!")
    print(f"Total records: {collection_count}")
    print(f"Results file: {results_file}")
    print(f"{'='*60}")

    return results_file


def main():
    parser = argparse.ArgumentParser(
        description="Latency Sweep Orchestrator",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
    # Deploy Phase 1 (EU regions)
    python run_sweep.py --deploy --phase 1

    # Collect metrics for 1 hour
    python run_sweep.py --collect --warmup 300 --duration 3600

    # Full workflow
    python run_sweep.py --deploy --collect --phase 1 --duration 3600

    # Tear down
    python run_sweep.py --destroy --phase 1
        """
    )

    parser.add_argument("--deploy", action="store_true", help="Deploy infrastructure")
    parser.add_argument("--collect", action="store_true", help="Collect metrics")
    parser.add_argument("--destroy", action="store_true", help="Destroy infrastructure")
    parser.add_argument("--phase", type=int, default=1, choices=[1, 2],
                        help="Deployment phase: 1=EU, 2=EU+Asia/ME")
    parser.add_argument("--warmup", type=int, default=300,
                        help="Warmup duration in seconds (default: 300)")
    parser.add_argument("--duration", type=int, default=3600,
                        help="Measurement duration in seconds (default: 3600)")
    parser.add_argument("--poll-interval", type=int, default=60,
                        help="Metrics polling interval in seconds (default: 60)")
    parser.add_argument("--output-dir", type=Path, default=Path("./results"),
                        help="Output directory for results")
    parser.add_argument("--ssh-key", type=str,
                        help="SSH public key (or set TF_VAR_ssh_public_key)")
    parser.add_argument("--experiment-id", type=str,
                        help="Experiment ID (auto-generated if not provided)")

    args = parser.parse_args()

    if not any([args.deploy, args.collect, args.destroy]):
        parser.print_help()
        sys.exit(1)

    experiment_id = args.experiment_id or f"latency-sweep-{datetime.utcnow().strftime('%Y%m%d-%H%M')}"

    # Deploy
    if args.deploy:
        print(f"\n{'='*60}")
        print(f"Deploying infrastructure (Phase {args.phase})")
        print(f"{'='*60}\n")

        run_terraform("init")

        extra_vars = {"experiment_id": experiment_id}
        if args.ssh_key:
            extra_vars["ssh_public_key"] = args.ssh_key

        run_terraform("apply", phase=args.phase, extra_vars=extra_vars)

    # Collect
    if args.collect:
        outputs = get_terraform_outputs()
        probes = parse_probe_endpoints(outputs)

        if not probes:
            print("ERROR: No probes found. Did you run --deploy first?")
            sys.exit(1)

        # Use experiment ID from outputs if available
        tf_experiment_id = outputs.get("experiment_id", {}).get("value", experiment_id)

        if not wait_for_probes(probes):
            print("ERROR: Probes not ready. Check AWS console for instance status.")
            sys.exit(1)

        config = SweepConfig(
            experiment_id=tf_experiment_id,
            warmup_sec=args.warmup,
            measurement_sec=args.duration,
            poll_interval_sec=args.poll_interval,
            output_dir=args.output_dir
        )

        results_file = run_collection(config, probes)
        print(f"\nTo analyze results, run:")
        print(f"  python analyze.py {results_file}")

    # Destroy
    if args.destroy:
        print(f"\n{'='*60}")
        print(f"Destroying infrastructure")
        print(f"{'='*60}\n")

        run_terraform("destroy", phase=args.phase)


if __name__ == "__main__":
    main()
