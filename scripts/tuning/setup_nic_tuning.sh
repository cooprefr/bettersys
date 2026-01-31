#!/bin/bash
# =============================================================================
# NIC Tuning Script - Ring Sizes, RPS/XPS, Offloads
# Purpose: Optimize NIC for low-latency market data reception
# =============================================================================

set -e

IFACE="${1:-eth0}"
RPS_CPUS="${2:-0c}"  # Bitmask for RPS (default: cores 2,3 = 0x0C)

echo "=== NIC Tuning for $IFACE ==="

# Check if interface exists
if [ ! -d "/sys/class/net/$IFACE" ]; then
    echo "ERROR: Interface $IFACE does not exist"
    echo "Available interfaces:"
    ls /sys/class/net/
    exit 1
fi

# Check for ethtool
if ! command -v ethtool &> /dev/null; then
    echo "ERROR: ethtool not found. Install with: sudo apt install ethtool"
    exit 1
fi

# === Ring Sizes ===
echo ""
echo "=== Configuring Ring Sizes ==="
echo "Current ring sizes:"
ethtool -g $IFACE 2>/dev/null || echo "  (ethtool -g not supported)"

# Set ring sizes to 2048 (balance between burst absorption and latency)
echo "Setting ring sizes to 2048..."
sudo ethtool -G $IFACE rx 2048 tx 2048 2>/dev/null || echo "  (ring size change not supported)"

echo "New ring sizes:"
ethtool -g $IFACE 2>/dev/null | grep -A4 "Current" || true

# === Interrupt Coalescing ===
echo ""
echo "=== Configuring Interrupt Coalescing ==="
echo "Current coalescing settings:"
ethtool -c $IFACE 2>/dev/null || echo "  (coalescing not supported)"

# Low latency: reduce coalescing (may increase CPU usage)
echo "Setting low-latency coalescing..."
sudo ethtool -C $IFACE rx-usecs 10 tx-usecs 10 2>/dev/null || echo "  (coalescing change not supported)"
sudo ethtool -C $IFACE adaptive-rx off adaptive-tx off 2>/dev/null || true

# === Hardware Offloads ===
echo ""
echo "=== Configuring Hardware Offloads ==="
echo "Current offload settings:"
ethtool -k $IFACE 2>/dev/null | head -20 || echo "  (offload query not supported)"

# Enable useful offloads, disable problematic ones
echo "Optimizing offloads..."
sudo ethtool -K $IFACE tso off 2>/dev/null || true   # TSO can add latency
sudo ethtool -K $IFACE gso off 2>/dev/null || true   # GSO can add latency
sudo ethtool -K $IFACE gro off 2>/dev/null || true   # GRO can add latency
sudo ethtool -K $IFACE lro off 2>/dev/null || true   # LRO can add latency
sudo ethtool -K $IFACE rx-checksumming on 2>/dev/null || true  # HW checksum is fast
sudo ethtool -K $IFACE tx-checksumming on 2>/dev/null || true

# === Hardware Timestamping ===
echo ""
echo "=== Checking Hardware Timestamping ==="
ethtool -T $IFACE 2>/dev/null || echo "  (timestamping query not supported)"

# Try to enable hardware timestamping
sudo ethtool -K $IFACE rx-hardware-timestamp on 2>/dev/null || true

# === RPS (Receive Packet Steering) ===
echo ""
echo "=== Configuring RPS ==="
NUM_QUEUES=$(ls -d /sys/class/net/$IFACE/queues/rx-* 2>/dev/null | wc -l)

if [ "$NUM_QUEUES" -gt 0 ]; then
    echo "Found $NUM_QUEUES RX queues"
    echo "Setting RPS CPU mask to $RPS_CPUS..."
    
    for i in $(seq 0 $((NUM_QUEUES-1))); do
        rps_file="/sys/class/net/$IFACE/queues/rx-$i/rps_cpus"
        if [ -f "$rps_file" ]; then
            echo "$RPS_CPUS" | sudo tee "$rps_file" > /dev/null
            echo "  rx-$i: $RPS_CPUS"
        fi
    done
    
    # Set RPS flow count per queue
    echo "Setting RPS flow count..."
    for i in $(seq 0 $((NUM_QUEUES-1))); do
        flow_file="/sys/class/net/$IFACE/queues/rx-$i/rps_flow_cnt"
        if [ -f "$flow_file" ]; then
            echo 4096 | sudo tee "$flow_file" > /dev/null
        fi
    done
else
    echo "No RX queues found (single-queue NIC or virtual interface)"
fi

# === XPS (Transmit Packet Steering) ===
echo ""
echo "=== Configuring XPS ==="
NUM_TX_QUEUES=$(ls -d /sys/class/net/$IFACE/queues/tx-* 2>/dev/null | wc -l)

if [ "$NUM_TX_QUEUES" -gt 0 ]; then
    echo "Found $NUM_TX_QUEUES TX queues"
    echo "Setting XPS CPU masks..."
    
    # Distribute TX queues across cores
    # Core 2 = 0x04, Core 3 = 0x08
    masks=("04" "08" "04" "08" "04" "08" "04" "08")
    
    for i in $(seq 0 $((NUM_TX_QUEUES-1))); do
        xps_file="/sys/class/net/$IFACE/queues/tx-$i/xps_cpus"
        if [ -f "$xps_file" ]; then
            mask_idx=$((i % ${#masks[@]}))
            echo "${masks[$mask_idx]}" | sudo tee "$xps_file" > /dev/null
            echo "  tx-$i: ${masks[$mask_idx]}"
        fi
    done
else
    echo "No TX queues found"
fi

# === Verification ===
echo ""
echo "=== Final Configuration ==="

echo ""
echo "Ring sizes:"
ethtool -g $IFACE 2>/dev/null | grep -A4 "Current" || true

echo ""
echo "Interface statistics (drops/errors):"
ethtool -S $IFACE 2>/dev/null | grep -iE "drop|miss|error|fifo|overrun" | head -15 || \
    ip -s link show $IFACE | grep -E "RX|TX|errors|dropped"

echo ""
echo "=== NIC tuning complete ==="
