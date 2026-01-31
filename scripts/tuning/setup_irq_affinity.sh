#!/bin/bash
# =============================================================================
# IRQ Affinity Setup for Low-Latency Network Processing
# Purpose: Pin NIC interrupts to specific cores, away from application cores
# =============================================================================

set -e

IFACE="${1:-eth0}"
IRQ_CORES="${2:-2,3}"  # Cores to pin IRQs to (comma-separated)

echo "=== IRQ Affinity Setup for $IFACE ==="
echo "Target cores: $IRQ_CORES"

# Check if interface exists
if [ ! -d "/sys/class/net/$IFACE" ]; then
    echo "ERROR: Interface $IFACE does not exist"
    echo "Available interfaces:"
    ls /sys/class/net/
    exit 1
fi

# Disable irqbalance (conflicts with manual affinity)
if systemctl is-active --quiet irqbalance 2>/dev/null; then
    echo "Stopping irqbalance service..."
    sudo systemctl stop irqbalance
    sudo systemctl disable irqbalance
    echo "irqbalance disabled"
fi

# Find IRQs for this interface
echo ""
echo "Finding IRQs for $IFACE..."
IRQS=$(grep "$IFACE" /proc/interrupts 2>/dev/null | awk -F: '{print $1}' | tr -d ' ')

if [ -z "$IRQS" ]; then
    echo "WARNING: No IRQs found for $IFACE in /proc/interrupts"
    echo "Trying alternative pattern..."
    IRQS=$(grep -E "${IFACE}[-_]" /proc/interrupts 2>/dev/null | awk -F: '{print $1}' | tr -d ' ')
fi

if [ -z "$IRQS" ]; then
    echo "ERROR: Could not find IRQs for $IFACE"
    echo "Current interrupt mappings:"
    cat /proc/interrupts | head -30
    exit 1
fi

# Convert cores to array
IFS=',' read -ra CORE_ARRAY <<< "$IRQ_CORES"
NUM_CORES=${#CORE_ARRAY[@]}

echo ""
echo "Found $(echo "$IRQS" | wc -w) IRQs, distributing across $NUM_CORES cores"

# Pin each IRQ to cores in round-robin
i=0
for irq in $IRQS; do
    core_idx=$((i % NUM_CORES))
    core=${CORE_ARRAY[$core_idx]}
    
    if [ -f "/proc/irq/$irq/smp_affinity_list" ]; then
        echo "Pinning IRQ $irq to core $core"
        echo "$core" | sudo tee /proc/irq/$irq/smp_affinity_list > /dev/null
    else
        echo "WARNING: /proc/irq/$irq/smp_affinity_list not found"
    fi
    
    ((i++))
done

echo ""
echo "=== Verification ==="
echo "IRQ affinity settings for $IFACE:"
for irq in $IRQS; do
    if [ -f "/proc/irq/$irq/smp_affinity_list" ]; then
        affinity=$(cat /proc/irq/$irq/smp_affinity_list)
        echo "  IRQ $irq -> Core(s) $affinity"
    fi
done

echo ""
echo "Current interrupt counts:"
grep "$IFACE" /proc/interrupts | head -10

echo ""
echo "=== IRQ affinity setup complete ==="
