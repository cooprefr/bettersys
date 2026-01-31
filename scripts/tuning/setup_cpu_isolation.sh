#!/bin/bash
# =============================================================================
# CPU Isolation Setup Script
# Purpose: Configure CPU isolation for low-latency application processing
# 
# NOTE: Full isolation requires kernel cmdline changes (GRUB) and reboot.
#       This script sets runtime-configurable options and generates the
#       required GRUB configuration.
# =============================================================================

set -e

APP_CORES="${1:-4,5,6,7}"      # Cores for application (isolated from kernel)
IRQ_CORES="${2:-2,3}"          # Cores for IRQ processing

echo "=== CPU Isolation Setup ==="
echo "Application cores: $APP_CORES"
echo "IRQ processing cores: $IRQ_CORES"

# === Display Current CPU Topology ===
echo ""
echo "=== Current CPU Topology ==="
lscpu | grep -E "CPU\(s\)|Thread|Core|Socket|NUMA" || true

if [ -f /proc/cpuinfo ]; then
    echo ""
    echo "Physical cores:"
    grep -E "processor|core id" /proc/cpuinfo | paste - - | head -16
fi

# === Check if cpupower is available ===
echo ""
echo "=== CPU Governor Setup ==="

if command -v cpupower &> /dev/null; then
    echo "Current governor:"
    cpupower frequency-info | grep "current CPU frequency" || true
    
    echo ""
    echo "Setting performance governor..."
    sudo cpupower frequency-set -g performance 2>/dev/null || \
        echo "  (governor change failed - may require kernel support)"
    
    echo "New governor:"
    cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || \
        echo "  (scaling_governor not available)"
else
    echo "cpupower not found. Install with: sudo apt install linux-tools-common"
    
    # Try direct sysfs approach
    for cpu_dir in /sys/devices/system/cpu/cpu[0-9]*; do
        gov_file="$cpu_dir/cpufreq/scaling_governor"
        if [ -f "$gov_file" ]; then
            echo performance | sudo tee "$gov_file" > /dev/null 2>&1 || true
        fi
    done
    echo "Attempted direct sysfs governor change"
fi

# === Disable CPU Frequency Scaling (Turbo) ===
echo ""
echo "=== Intel Turbo / AMD Boost ==="

# Intel
if [ -f /sys/devices/system/cpu/intel_pstate/no_turbo ]; then
    current=$(cat /sys/devices/system/cpu/intel_pstate/no_turbo)
    echo "Intel Turbo disabled: $current (0=turbo on, 1=turbo off)"
    # Uncomment to disable turbo (reduces jitter but lowers peak freq):
    # echo 1 | sudo tee /sys/devices/system/cpu/intel_pstate/no_turbo
fi

# AMD
if [ -f /sys/devices/system/cpu/cpufreq/boost ]; then
    current=$(cat /sys/devices/system/cpu/cpufreq/boost)
    echo "AMD Boost enabled: $current (1=on, 0=off)"
fi

# === Generate GRUB Configuration ===
echo ""
echo "=== GRUB Kernel Command Line ==="
echo ""
echo "For full CPU isolation, add the following to /etc/default/grub:"
echo ""

# Determine all isolated cores (app cores)
ALL_ISOLATED="$APP_CORES"

cat << EOF
GRUB_CMDLINE_LINUX="\$GRUB_CMDLINE_LINUX isolcpus=$ALL_ISOLATED nohz_full=$ALL_ISOLATED rcu_nocbs=$ALL_ISOLATED"
EOF

echo ""
echo "Optional: For maximum isolation, also add:"
echo "  processor.max_cstate=1 intel_idle.max_cstate=0  # Disable C-states (higher power)"
echo "  idle=poll                                        # Busy-wait idle (highest power)"
echo "  nosoftlockup                                     # Disable soft lockup detector"
echo "  nmi_watchdog=0                                   # Disable NMI watchdog"

echo ""
echo "After editing GRUB, run:"
echo "  sudo update-grub && sudo reboot"

# === Runtime Scheduler Tuning ===
echo ""
echo "=== Runtime Scheduler Tuning ==="

# Reduce scheduler migration cost
if [ -f /proc/sys/kernel/sched_migration_cost_ns ]; then
    current=$(cat /proc/sys/kernel/sched_migration_cost_ns)
    echo "sched_migration_cost_ns: $current"
    # Increase to reduce migrations (better cache locality)
    echo 5000000 | sudo tee /proc/sys/kernel/sched_migration_cost_ns > /dev/null 2>&1 || true
fi

# Reduce scheduler wake-up granularity
if [ -f /proc/sys/kernel/sched_wakeup_granularity_ns ]; then
    current=$(cat /proc/sys/kernel/sched_wakeup_granularity_ns)
    echo "sched_wakeup_granularity_ns: $current"
fi

# === Move Kernel Threads Away from App Cores ===
echo ""
echo "=== Moving Kernel Threads ==="

# This is best-effort at runtime; proper isolation requires isolcpus
echo "Moving kernel threads to cores 0,1 (IRQ cores: $IRQ_CORES)..."

# Move RCU callbacks
for rcu in /sys/kernel/rcu_*/rcu_cpu_kthread_cpu 2>/dev/null; do
    if [ -f "$rcu" ]; then
        echo 0 | sudo tee "$rcu" > /dev/null 2>&1 || true
    fi
done

# Move workqueue threads (best effort)
if [ -d /sys/devices/virtual/workqueue ]; then
    for wq in /sys/devices/virtual/workqueue/*/cpumask; do
        if [ -f "$wq" ]; then
            # Set to IRQ cores only (e.g., 0x0F for cores 0-3)
            echo "0f" | sudo tee "$wq" > /dev/null 2>&1 || true
        fi
    done
fi

# === Verification ===
echo ""
echo "=== Verification ==="

echo ""
echo "Current isolated CPUs:"
cat /sys/devices/system/cpu/isolated 2>/dev/null || echo "  (none - requires isolcpus kernel param)"

echo ""
echo "Current nohz_full CPUs:"
cat /sys/devices/system/cpu/nohz_full 2>/dev/null || echo "  (none - requires nohz_full kernel param)"

echo ""
echo "CPU governor per core:"
for cpu in /sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_governor; do
    if [ -f "$cpu" ]; then
        cpu_num=$(echo "$cpu" | grep -oE 'cpu[0-9]+')
        gov=$(cat "$cpu")
        echo "  $cpu_num: $gov"
    fi
done | head -8

echo ""
echo "=== CPU isolation setup complete ==="
echo ""
echo "IMPORTANT: Full isolation requires kernel cmdline changes and reboot."
echo "See GRUB configuration above."
