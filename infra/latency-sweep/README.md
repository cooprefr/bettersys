# Binance Latency Sweep

Automated multi-region, multi-instance sweep to find the optimal AWS deployment for minimizing p99 market data latency to Binance.

## Quick Start

```bash
# 1. Configure (copy and edit)
cd terraform
cp terraform.tfvars.example terraform.tfvars
# Edit terraform.tfvars with your SSH key

# 2. Deploy Phase 1 (EU regions)
cd ../orchestrate
python run_sweep.py --deploy --phase 1

# 3. Collect metrics (1 hour)
python run_sweep.py --collect --warmup 300 --duration 3600

# 4. Analyze results
python analyze.py results/latency-sweep-*.jsonl

# 5. Tear down
python run_sweep.py --destroy --phase 1
```

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    Orchestrator (local)                      │
│  run_sweep.py → Terraform → analyze.py                      │
└─────────────────────┬───────────────────────────────────────┘
                      │ HTTP /metrics
    ┌─────────────────┼─────────────────┐
    │                 │                 │
    ▼                 ▼                 ▼
┌────────┐      ┌────────┐        ┌────────┐
│ eu-w-1 │      │ eu-c-1 │        │ eu-w-2 │   ... more regions
│ c6i    │      │ c6i    │        │ c6i    │
│ c6in   │      │ c6in   │        │ c6in   │
└───┬────┘      └───┬────┘        └───┬────┘
    │               │                 │
    └───────────────┼─────────────────┘
                    │ WebSocket
                    ▼
            ┌──────────────┐
            │   Binance    │
            │  stream.b... │
            └──────────────┘
```

## Directory Structure

```
infra/latency-sweep/
├── terraform/           # Infrastructure as Code
│   ├── main.tf          # Core configuration
│   ├── variables.tf     # Input variables
│   ├── providers.tf     # Multi-region AWS providers
│   ├── probes.tf        # Probe deployments per region
│   ├── outputs.tf       # Terraform outputs
│   └── modules/probe/   # Reusable probe module
├── bootstrap/
│   └── install.sh.tpl   # EC2 user data template
├── orchestrate/
│   ├── run_sweep.py     # Deploy + collect orchestrator
│   ├── analyze.py       # Results analysis + recommendation
│   └── requirements.txt # Python dependencies
└── README.md
```

## Test Matrix

### Regions

| Phase | Region | Code | Rationale |
|-------|--------|------|-----------|
| 1 | Ireland | eu-west-1 | Major AWS hub |
| 1 | Frankfurt | eu-central-1 | DE-CIX, financial hub |
| 1 | London | eu-west-2 | LINX, financial hub |
| 2 | Singapore | ap-southeast-1 | Likely closest to Binance |
| 2 | UAE | me-central-1 | Test newer region |
| 2 | Tokyo | ap-northeast-1 | Alternative Asia PoP |

### Instance Types

| Key | Type | Network | Notes |
|-----|------|---------|-------|
| c6i_large | c6i.large | Up to 12.5 Gbps | Baseline compute |
| c6in_large | c6in.large | Up to 25 Gbps + ENA Express | Network optimized |
| c7gn_medium | c7gn.medium | Up to 25 Gbps | Graviton3 + enhanced NW |

## Decision Rule

**Score Formula:**
```
SCORE = 0.7 × normalized_p99 + 0.3 × normalized_cv
```

Lower score = better. The winner must also pass hard constraints:
- p99 < 50ms
- Reconnects < 1/hour
- p999/p99 ratio < 3.0

Statistical validation:
- Mann-Whitney U test (p < 0.05)
- Bootstrap 95% CI for p99 difference
- Minimum 3 independent runs recommended

## Metrics Collected

| Metric | Description |
|--------|-------------|
| p99_us | 99th percentile one-way latency |
| p999_us | 99.9th percentile (tail) |
| cv | Coefficient of variation (stability) |
| reconnects | WebSocket reconnection count |
| messages | Total messages received |

## Cost Estimate

| Phase | Probes | Instance Hours | Est. Cost |
|-------|--------|---------------|-----------|
| 1 | 6 (3 regions × 2 types) | ~4h each | ~$5 |
| 2 | 12 (6 regions × 2 types) | ~4h each | ~$10 |

## Troubleshooting

**Probes not becoming healthy:**
```bash
# Check instance status
aws ec2 describe-instances --filters "Name=tag:Project,Values=betterbot-latency-sweep"

# SSH to probe
ssh -i ~/.ssh/your-key ec2-user@<public-ip>

# Check service status
sudo systemctl status binance-probe
sudo journalctl -u binance-probe -f
```

**Terraform state issues:**
```bash
cd terraform
terraform state list
terraform refresh
```

## References

- [Experiment Design Plan](../../docs/BINANCE_LATENCY_SWEEP_PLAN.md)
- [Binance WebSocket Streams](https://binance-docs.github.io/apidocs/spot/en/#websocket-market-streams)
- [AWS Enhanced Networking](https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/enhanced-networking.html)
