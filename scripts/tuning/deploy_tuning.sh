#!/bin/bash
# =============================================================================
# Master Deployment Script for Low-Latency Tuning
# Purpose: Apply all tuning settings in the correct order
#
# Usage: sudo ./deploy_tuning.sh [INTERFACE] [APP_CORES] [IRQ_CORES]
#        sudo ./deploy_tuning.sh eth0 4,5,6,7 2,3
#
# Requirements:
#   - Root privileges (for sysctl, IRQ affinity)
#   - ethtool (for NIC tuning)
#   - cpupower (optional, for CPU governor)
# =============================================================================

set -e

# Configuration
IFACE="${1:-eth0}"
APP_CORES="${2:-4,5,6,7}"
IRQ_CORES="${3:-2,3}"

# Derived settings
RPS_CPUS="0c"  # Bitmask for cores 2,3 (0x04 + 0x08 = 0x0C)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "============================================================"
echo "        LOW-LATENCY TUNING DEPLOYMENT"
echo "        $(date)"
echo "============================================================"
echo ""
echo "Configuration:"
echo "  Network Interface: $IFACE"
echo "  Application Cores: $APP_CORES (isolated)"
echo "  IRQ Cores:         $IRQ_CORES"
echo "  RPS CPU Mask:      $RPS_CPUS"
echo ""

# Check for root
if [ "$EUID" -ne 0 ]; then
    echo "ERROR: This script must be run as root"
    echo "Usage: sudo $0 $IFACE $APP_CORES $IRQ_CORES"
    exit 1
fi

# Check interface exists
if [ ! -d "/sys/class/net/$IFACE" ]; then
    echo "ERROR: Interface $IFACE does not exist"
    echo "Available interfaces:"
    ls /sys/class/net/
    exit 1
fi

# === Step 1: Apply Sysctl Settings ===
echo ""
echo "=== Step 1/5: Applying Sysctl Settings ==="
SYSCTL_CONF="$SCRIPT_DIR/99-market-data.conf"

if [ -f "$SYSCTL_CONF" ]; then
    echo "Copying $SYSCTL_CONF to /etc/sysctl.d/"
    cp "$SYSCTL_CONF" /etc/sysctl.d/
    
    echo "Applying sysctl settings..."
    sysctl -p /etc/sysctl.d/99-market-data.conf
    echo "Sysctl settings applied"
else
    echo "WARNING: $SYSCTL_CONF not found, skipping"
fi

# === Step 2: NIC Tuning ===
echo ""
echo "=== Step 2/5: Applying NIC Tuning ==="
NIC_SCRIPT="$SCRIPT_DIR/setup_nic_tuning.sh"

if [ -f "$NIC_SCRIPT" ]; then
    chmod +x "$NIC_SCRIPT"
    bash "$NIC_SCRIPT" "$IFACE" "$RPS_CPUS"
else
    echo "WARNING: $NIC_SCRIPT not found, applying inline..."
    
    # Ring sizes
    ethtool -G "$IFACE" rx 2048 tx 2048 2>/dev/null || true
    
    # Coalescing
    ethtool -C "$IFACE" rx-usecs 10 tx-usecs 10 2>/dev/null || true
    ethtool -C "$IFACE" adaptive-rx off adaptive-tx off 2>/dev/null || true
    
    # Offloads (disable latency-adding features)
    ethtool -K "$IFACE" tso off gso off gro off lro off 2>/dev/null || true
fi

# === Step 3: IRQ Affinity ===
echo ""
echo "=== Step 3/5: Configuring IRQ Affinity ==="
IRQ_SCRIPT="$SCRIPT_DIR/setup_irq_affinity.sh"

if [ -f "$IRQ_SCRIPT" ]; then
    chmod +x "$IRQ_SCRIPT"
    bash "$IRQ_SCRIPT" "$IFACE" "$IRQ_CORES"
else
    echo "WARNING: $IRQ_SCRIPT not found, applying inline..."
    
    # Stop irqbalance
    systemctl stop irqbalance 2>/dev/null || true
    systemctl disable irqbalance 2>/dev/null || true
    
    # Pin IRQs
    IFS=',' read -ra CORES <<< "$IRQ_CORES"
    i=0
    for irq in $(grep "$IFACE" /proc/interrupts | awk -F: '{print $1}' | tr -d ' '); do
        core_idx=$((i % ${#CORES[@]}))
        echo "${CORES[$core_idx]}" > /proc/irq/$irq/smp_affinity_list 2>/dev/null || true
        ((i++))
    done
fi

# === Step 4: CPU Tuning ===
echo ""
echo "=== Step 4/5: Applying CPU Tuning ==="
CPU_SCRIPT="$SCRIPT_DIR/setup_cpu_isolation.sh"

if [ -f "$CPU_SCRIPT" ]; then
    chmod +x "$CPU_SCRIPT"
    bash "$CPU_SCRIPT" "$APP_CORES" "$IRQ_CORES"
else
    echo "WARNING: $CPU_SCRIPT not found, applying inline..."
    
    # Set performance governor
    if command -v cpupower &> /dev/null; then
        cpupower frequency-set -g performance 2>/dev/null || true
    else
        for cpu in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
            echo performance > "$cpu" 2>/dev/null || true
        done
    fi
fi

# === Step 5: Enable BBR Congestion Control (Optional) ===
echo ""
echo "=== Step 5/5: Configuring Congestion Control ==="

# Check if BBR is available
if grep -q "bbr" /proc/sys/net/ipv4/tcp_available_congestion_control 2>/dev/null; then
    echo "BBR is available"
    
    # Ask user preference (default to cubic for safety)
    echo "Current congestion control: $(sysctl -n net.ipv4.tcp_congestion_control)"
    echo ""
    echo "BBR is recommended for WAN connections (variable latency)"
    echo "CUBIC is recommended for DC/LAN connections (low latency)"
    echo ""
    echo "To enable BBR, run:"
    echo "  sysctl -w net.ipv4.tcp_congestion_control=bbr"
    echo "  sysctl -w net.core.default_qdisc=fq"
else
    # Try to load BBR module
    modprobe tcp_bbr 2>/dev/null || true
    if grep -q "bbr" /proc/sys/net/ipv4/tcp_available_congestion_control 2>/dev/null; then
        echo "BBR module loaded successfully"
    else
        echo "BBR not available on this kernel (using cubic)"
    fi
fi

# === Validation ===
echo ""
echo "============================================================"
echo "               DEPLOYMENT VALIDATION"
echo "============================================================"

VALIDATE_SCRIPT="$SCRIPT_DIR/validate_tuning.sh"
if [ -f "$VALIDATE_SCRIPT" ]; then
    chmod +x "$VALIDATE_SCRIPT"
    bash "$VALIDATE_SCRIPT" "$IFACE"
else
    echo "Running inline validation..."
    
    echo ""
    echo "Sysctl values:"
    sysctl net.core.rmem_max net.core.busy_poll net.ipv4.tcp_slow_start_after_idle
    
    echo ""
    echo "NIC ring sizes:"
    ethtool -g "$IFACE" 2>/dev/null | grep -A4 "Current" || true
    
    echo ""
    echo "IRQ affinity (first 4):"
    for irq in $(grep "$IFACE" /proc/interrupts | awk -F: '{print $1}' | tr -d ' ' | head -4); do
        echo "  IRQ $irq: $(cat /proc/irq/$irq/smp_affinity_list 2>/dev/null)"
    done
    
    echo ""
    echo "CPU governor:"
    cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo "N/A"
fi

# === Persistence Reminder ===
echo ""
echo "============================================================"
echo "               IMPORTANT: PERSISTENCE"
echo "============================================================"
echo ""
echo "Sysctl settings: PERSISTENT (via /etc/sysctl.d/)"
echo "IRQ affinity:    NOT PERSISTENT (reapply after reboot)"
echo "NIC settings:    NOT PERSISTENT (reapply after reboot)"
echo "CPU governor:    NOT PERSISTENT (reapply after reboot)"
echo "CPU isolation:   REQUIRES GRUB CHANGES (see output above)"
echo ""
echo "To persist IRQ/NIC/CPU settings, add this script to:"
echo "  /etc/rc.local"
echo "  or create a systemd service"
echo ""
echo "Example systemd service:"
echo "  /etc/systemd/system/low-latency-tuning.service"
echo ""

# === Create Systemd Service File ===
SERVICE_FILE="/etc/systemd/system/low-latency-tuning.service"
echo "Creating systemd service at $SERVICE_FILE..."

cat > "$SERVICE_FILE" << EOF
[Unit]
Description=Low-Latency Network Tuning
After=network.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=$SCRIPT_DIR/deploy_tuning.sh $IFACE $APP_CORES $IRQ_CORES

[Install]
WantedBy=multi-user.target
EOF

echo "Systemd service created."
echo ""
echo "To enable on boot:"
echo "  sudo systemctl enable low-latency-tuning.service"
echo ""
echo "============================================================"
echo "        DEPLOYMENT COMPLETE"
echo "============================================================"
