#!/bin/bash
# =============================================================================
# Tuning Validation Script
# Purpose: Verify all low-latency tuning settings and measure before/after
# =============================================================================

set -e

IFACE="${1:-eth0}"
SERVER="${2:-}"  # Optional: server to check active connection to

echo "============================================================"
echo "        LOW-LATENCY TUNING VALIDATION REPORT"
echo "        $(date)"
echo "============================================================"

# === System Info ===
echo ""
echo "=== System Information ==="
uname -a
echo ""
lscpu | grep -E "Model name|CPU\(s\)|Thread|Core|Socket" | head -5

# === CPU Settings ===
echo ""
echo "=== CPU Configuration ==="

echo ""
echo "Isolated CPUs:"
cat /sys/devices/system/cpu/isolated 2>/dev/null || echo "  (none configured)"

echo ""
echo "nohz_full CPUs:"
cat /sys/devices/system/cpu/nohz_full 2>/dev/null || echo "  (none configured)"

echo ""
echo "CPU Governors:"
for cpu in /sys/devices/system/cpu/cpu[0-7]/cpufreq/scaling_governor 2>/dev/null; do
    if [ -f "$cpu" ]; then
        cpu_num=$(echo "$cpu" | grep -oE 'cpu[0-9]+')
        gov=$(cat "$cpu")
        printf "  %-6s: %s\n" "$cpu_num" "$gov"
    fi
done

echo ""
echo "Current CPU Frequencies (MHz):"
for cpu in /sys/devices/system/cpu/cpu[0-7]/cpufreq/scaling_cur_freq 2>/dev/null; do
    if [ -f "$cpu" ]; then
        cpu_num=$(echo "$cpu" | grep -oE 'cpu[0-9]+')
        freq=$(($(cat "$cpu") / 1000))
        printf "  %-6s: %d MHz\n" "$cpu_num" "$freq"
    fi
done

# === Network Sysctls ===
echo ""
echo "=== Network Sysctls ==="

sysctls=(
    "net.core.rmem_max"
    "net.core.wmem_max"
    "net.core.rmem_default"
    "net.core.wmem_default"
    "net.core.netdev_max_backlog"
    "net.core.somaxconn"
    "net.core.busy_poll"
    "net.core.busy_read"
    "net.core.rps_sock_flow_entries"
    "net.ipv4.tcp_rmem"
    "net.ipv4.tcp_wmem"
    "net.ipv4.tcp_congestion_control"
    "net.ipv4.tcp_slow_start_after_idle"
    "net.ipv4.tcp_fin_timeout"
    "net.ipv4.tcp_timestamps"
    "net.ipv4.tcp_sack"
)

for sysctl in "${sysctls[@]}"; do
    value=$(sysctl -n "$sysctl" 2>/dev/null || echo "N/A")
    printf "  %-40s = %s\n" "$sysctl" "$value"
done

# === NIC Configuration ===
echo ""
echo "=== NIC Configuration ($IFACE) ==="

if [ -d "/sys/class/net/$IFACE" ]; then
    echo ""
    echo "Ring Sizes:"
    ethtool -g "$IFACE" 2>/dev/null | grep -A4 "Current" || echo "  (not available)"
    
    echo ""
    echo "Coalescing:"
    ethtool -c "$IFACE" 2>/dev/null | grep -E "rx-usecs|tx-usecs|adaptive" | head -5 || echo "  (not available)"
    
    echo ""
    echo "Offload Status:"
    ethtool -k "$IFACE" 2>/dev/null | grep -E "tcp-segmentation|generic-segmentation|generic-receive|large-receive" || echo "  (not available)"
    
    echo ""
    echo "Interface Statistics (drops/errors):"
    ethtool -S "$IFACE" 2>/dev/null | grep -iE "drop|miss|error|fifo|overrun" | head -10 || \
        ip -s link show "$IFACE" 2>/dev/null | grep -E "RX|TX|errors|dropped"
else
    echo "Interface $IFACE not found"
fi

# === IRQ Affinity ===
echo ""
echo "=== IRQ Affinity ($IFACE) ==="

irqs=$(grep "$IFACE" /proc/interrupts 2>/dev/null | awk -F: '{print $1}' | tr -d ' ' | head -8)
if [ -n "$irqs" ]; then
    for irq in $irqs; do
        if [ -f "/proc/irq/$irq/smp_affinity_list" ]; then
            affinity=$(cat /proc/irq/$irq/smp_affinity_list)
            printf "  IRQ %-4s -> CPU(s) %s\n" "$irq" "$affinity"
        fi
    done
else
    echo "  No IRQs found for $IFACE"
fi

# === RPS/XPS ===
echo ""
echo "=== RPS/XPS Configuration ($IFACE) ==="

echo "RPS CPU masks:"
for rps in /sys/class/net/$IFACE/queues/rx-*/rps_cpus 2>/dev/null; do
    if [ -f "$rps" ]; then
        queue=$(echo "$rps" | grep -oE 'rx-[0-9]+')
        mask=$(cat "$rps")
        printf "  %-8s: %s\n" "$queue" "$mask"
    fi
done | head -8

echo ""
echo "XPS CPU masks:"
for xps in /sys/class/net/$IFACE/queues/tx-*/xps_cpus 2>/dev/null; do
    if [ -f "$xps" ]; then
        queue=$(echo "$xps" | grep -oE 'tx-[0-9]+')
        mask=$(cat "$xps")
        printf "  %-8s: %s\n" "$queue" "$mask"
    fi
done | head -8

# === Softnet Stats ===
echo ""
echo "=== Softnet Statistics ==="
echo "(columns: processed, dropped, time_squeeze, ...)"
echo "CPU   Processed      Dropped    Time-Squeeze"
cpu=0
while read line; do
    processed=$(echo "$line" | awk '{print "0x"$1}' | xargs printf "%d")
    dropped=$(echo "$line" | awk '{print "0x"$2}' | xargs printf "%d")
    time_squeeze=$(echo "$line" | awk '{print "0x"$3}' | xargs printf "%d")
    printf "%-5d %-14d %-10d %d\n" $cpu $processed $dropped $time_squeeze
    ((cpu++))
done < /proc/net/softnet_stat | head -8

# === Active Connections ===
if [ -n "$SERVER" ]; then
    echo ""
    echo "=== Active Connection to $SERVER ==="
    ss -ti dst "$SERVER" 2>/dev/null | head -20 || echo "  No active connection to $SERVER"
fi

# === irqbalance Status ===
echo ""
echo "=== irqbalance Status ==="
if systemctl is-active --quiet irqbalance 2>/dev/null; then
    echo "  WARNING: irqbalance is RUNNING (conflicts with manual IRQ affinity)"
else
    echo "  irqbalance is stopped/disabled (good)"
fi

# === Memory ===
echo ""
echo "=== Memory Configuration ==="
echo "Transparent Huge Pages:"
cat /sys/kernel/mm/transparent_hugepage/enabled 2>/dev/null || echo "  (not available)"

echo ""
echo "NUMA Balancing:"
cat /proc/sys/kernel/numa_balancing 2>/dev/null || echo "  (not available)"

# === Summary Score ===
echo ""
echo "============================================================"
echo "                    TUNING SCORE"
echo "============================================================"

score=0
max_score=0

# Check busy_poll
((max_score++))
busy_poll=$(sysctl -n net.core.busy_poll 2>/dev/null || echo 0)
if [ "$busy_poll" -gt 0 ]; then
    echo "[PASS] Busy-poll enabled ($busy_poll us)"
    ((score++))
else
    echo "[FAIL] Busy-poll not enabled"
fi

# Check slow_start_after_idle
((max_score++))
ssai=$(sysctl -n net.ipv4.tcp_slow_start_after_idle 2>/dev/null || echo 1)
if [ "$ssai" -eq 0 ]; then
    echo "[PASS] tcp_slow_start_after_idle disabled"
    ((score++))
else
    echo "[FAIL] tcp_slow_start_after_idle enabled (add latency on idle connections)"
fi

# Check CPU governor
((max_score++))
gov=$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo "unknown")
if [ "$gov" = "performance" ]; then
    echo "[PASS] CPU governor is 'performance'"
    ((score++))
else
    echo "[WARN] CPU governor is '$gov' (should be 'performance')"
fi

# Check irqbalance
((max_score++))
if ! systemctl is-active --quiet irqbalance 2>/dev/null; then
    echo "[PASS] irqbalance is disabled"
    ((score++))
else
    echo "[FAIL] irqbalance is running"
fi

# Check for drops
((max_score++))
drops=$(ethtool -S "$IFACE" 2>/dev/null | grep -iE "rx.*drop" | awk '{sum+=$2} END {print sum+0}')
if [ "$drops" -eq 0 ]; then
    echo "[PASS] No RX drops on $IFACE"
    ((score++))
else
    echo "[WARN] $drops RX drops detected on $IFACE"
fi

# Check buffer sizes
((max_score++))
rmem_max=$(sysctl -n net.core.rmem_max 2>/dev/null || echo 0)
if [ "$rmem_max" -ge 16777216 ]; then
    echo "[PASS] rmem_max >= 16MB ($rmem_max)"
    ((score++))
else
    echo "[WARN] rmem_max < 16MB ($rmem_max)"
fi

echo ""
echo "============================================================"
echo "              SCORE: $score / $max_score"
echo "============================================================"

if [ "$score" -eq "$max_score" ]; then
    echo "All tuning checks passed!"
elif [ "$score" -ge $((max_score - 2)) ]; then
    echo "Most tuning applied. Review warnings above."
else
    echo "Significant tuning missing. Run setup scripts."
fi
