# Low-Latency Tuning for Market Data Feeds

This directory contains production-ready tuning scripts for Linux hosts receiving high-frequency market data over TLS/WebSocket.

## Quick Start

```bash
# Deploy all tuning (requires root)
sudo ./deploy_tuning.sh eth0 4,5,6,7 2,3
```

Where:
- `eth0` - Network interface for market data
- `4,5,6,7` - CPU cores to isolate for application
- `2,3` - CPU cores for IRQ/network processing

## Scripts

| Script | Purpose |
|--------|---------|
| `deploy_tuning.sh` | Master deployment script (runs all others) |
| `99-market-data.conf` | Sysctl configuration for socket buffers, TCP, busy-poll |
| `setup_irq_affinity.sh` | Pin NIC interrupts to specific CPU cores |
| `setup_nic_tuning.sh` | Configure ring sizes, RPS/XPS, offloads |
| `setup_cpu_isolation.sh` | CPU governor + generates GRUB config for isolation |
| `validate_tuning.sh` | Verify all settings and generate report |

## What Each Tuning Does

### 1. Sysctl Settings (`99-market-data.conf`)

**Socket Buffers:**
- `net.core.rmem_max = 16777216` - 16MB max receive buffer
- `net.core.wmem_max = 16777216` - 16MB max send buffer
- `net.ipv4.tcp_rmem = 4096 1048576 16777216` - TCP receive buffer sizing
- `net.core.netdev_max_backlog = 50000` - NIC backlog for bursts

**Busy-Poll (trades CPU for latency):**
- `net.core.busy_poll = 50` - Poll for 50us before sleeping
- `net.core.busy_read = 50` - Same for reads

**TCP Optimizations:**
- `net.ipv4.tcp_slow_start_after_idle = 0` - Keep cwnd warm on idle connections
- `net.ipv4.tcp_nodelay` - Implicitly encouraged (app-level)
- `net.ipv4.tcp_timestamps = 1` - Better RTT estimation

### 2. IRQ Affinity

Pins NIC interrupts to specific cores to:
- Reduce cache thrashing from IRQ migration
- Keep application cores free from kernel interrupts
- Improve CPU cache locality for network processing

```bash
# Example: Pin to cores 2,3
./setup_irq_affinity.sh eth0 2,3
```

**Important:** Disables `irqbalance` which conflicts with manual pinning.

### 3. NIC Tuning

**Ring Sizes:**
- RX/TX rings set to 2048 entries
- Balance between burst absorption and latency
- Too large = more latency; too small = drops under burst

**Interrupt Coalescing:**
- `rx-usecs 10` / `tx-usecs 10` - Low coalescing for faster interrupts
- `adaptive-rx off` - Disable adaptive coalescing

**Offloads Disabled:**
- TSO, GSO, GRO, LRO - These add latency by batching
- Checksumming left ON (hardware is fast)

**RPS/XPS:**
- Receive Packet Steering distributes packets across CPUs
- Transmit Packet Steering aligns TX with RX

### 4. CPU Isolation

**Runtime Settings:**
- Performance governor (disable frequency scaling)
- Scheduler tuning (reduce migrations)

**Kernel Cmdline (requires reboot):**
```
isolcpus=4,5,6,7 nohz_full=4,5,6,7 rcu_nocbs=4,5,6,7
```

This:
- Removes isolated CPUs from scheduler load balancing
- Disables timer ticks on isolated CPUs (reduces jitter)
- Offloads RCU callbacks from isolated CPUs

### 5. Congestion Control

**BBR (recommended for WAN):**
- Model-based, not loss-based
- Maintains throughput during mild packet loss
- Better for variable-latency internet connections

**CUBIC (default for LAN/DC):**
- Loss-based
- Simpler, well-understood behavior
- Fine for clean datacenter networks

## Full CPU Isolation (Requires Reboot)

For maximum isolation, add to `/etc/default/grub`:

```bash
GRUB_CMDLINE_LINUX="isolcpus=4,5,6,7 nohz_full=4,5,6,7 rcu_nocbs=4,5,6,7"
```

Then:
```bash
sudo update-grub
sudo reboot
```

**Optional aggressive settings (higher power consumption):**
```
processor.max_cstate=1 intel_idle.max_cstate=0  # Disable C-states
idle=poll                                        # Busy-wait idle loop
```

## Rust Application Integration

The `socket_tuning` module in `rust-backend/src/performance/latency/` provides:

```rust
use crate::performance::latency::socket_tuning::{SocketTuningConfig, apply_socket_tuning};

// Apply to a TcpStream
let config = SocketTuningConfig::websocket();
let result = apply_socket_tuning(&tcp_stream, &config);
result.log_summary();
```

This sets:
- TCP_NODELAY (disable Nagle)
- SO_RCVBUF / SO_SNDBUF (buffer sizing)
- SO_BUSY_POLL (Linux busy polling)
- TCP_QUICKACK (immediate ACKs)
- SO_PRIORITY (traffic prioritization)

## Validation

Run the validation script to check all settings:

```bash
./validate_tuning.sh eth0
```

Output includes:
- Current sysctl values
- NIC configuration
- IRQ affinity mapping
- Softnet statistics (drops)
- Tuning score (pass/fail checks)

## Persistence

| Setting | Persists? | How to Persist |
|---------|-----------|----------------|
| Sysctl | Yes | Via `/etc/sysctl.d/` |
| IRQ affinity | No | Systemd service or rc.local |
| NIC tuning | No | Systemd service or rc.local |
| CPU governor | No | Systemd service or rc.local |
| CPU isolation | Yes (after reboot) | GRUB cmdline |

The deploy script creates a systemd service at `/etc/systemd/system/low-latency-tuning.service`. Enable it:

```bash
sudo systemctl enable low-latency-tuning.service
```

## Monitoring

After applying tuning, monitor for:

1. **Packet drops:**
   ```bash
   watch -n1 'ethtool -S eth0 | grep drop'
   ```

2. **Softnet stats (dropped packets per CPU):**
   ```bash
   watch -n1 'cat /proc/net/softnet_stat'
   ```

3. **IRQ distribution:**
   ```bash
   watch -n1 'cat /proc/interrupts | grep eth0'
   ```

4. **Application latency:**
   - Use the BetterBot latency histograms in the performance dashboard
   - Monitor `dome_ws_latency`, `binance_ws_latency` p50/p99

## Troubleshooting

**Drops under burst:**
- Increase ring sizes: `ethtool -G eth0 rx 4096 tx 4096`
- Increase socket buffers: `sysctl -w net.core.rmem_max=33554432`

**High CPU from busy-poll:**
- Reduce or disable: `sysctl -w net.core.busy_poll=0`

**Latency spikes after idle:**
- Verify: `sysctl net.ipv4.tcp_slow_start_after_idle` should be 0

**irqbalance conflicts:**
- Check: `systemctl status irqbalance`
- Disable: `systemctl disable --now irqbalance`

## References

- [Linux Kernel Networking Documentation](https://www.kernel.org/doc/html/latest/networking/)
- [Red Hat Performance Tuning Guide](https://access.redhat.com/documentation/en-us/red_hat_enterprise_linux/8/html/monitoring_and_managing_system_status_and_performance/)
- [BBR Congestion Control](https://cloud.google.com/blog/products/networking/tcp-bbr-congestion-control-comes-to-gcp-your-internet-just-got-faster)
