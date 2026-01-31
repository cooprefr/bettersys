#!/bin/bash
# =============================================================================
# Time Synchronization Validation Script
# =============================================================================
#
# Validates chrony/NTP configuration for HFT latency measurement.
# Run this before production deployment to ensure accurate timestamps.
#
# Usage: ./validate_time_sync.sh [--verbose]
#
# Exit codes:
#   0 - All checks passed
#   1 - One or more critical checks failed
#   2 - Warnings present but no critical failures

set -euo pipefail

VERBOSE="${1:-}"
WARNINGS=0
CRITICAL=0

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log_pass() {
    echo -e "${GREEN}✓${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}⚠${NC} $1"
    ((WARNINGS++)) || true
}

log_fail() {
    echo -e "${RED}✗${NC} $1"
    ((CRITICAL++)) || true
}

log_info() {
    echo "  $1"
}

# =============================================================================
# Check 1: Chrony is installed and running
# =============================================================================
echo ""
echo "=== Chrony Service Check ==="

if command -v chronyc &> /dev/null; then
    log_pass "chronyc is installed"
else
    log_fail "chronyc is not installed. Install with: apt-get install chrony"
    exit 1
fi

if systemctl is-active --quiet chronyd 2>/dev/null || systemctl is-active --quiet chrony 2>/dev/null; then
    log_pass "chrony service is running"
else
    log_fail "chrony service is not running. Start with: systemctl start chrony"
fi

# =============================================================================
# Check 2: NTP synchronization status
# =============================================================================
echo ""
echo "=== NTP Synchronization Status ==="

TRACKING=$(chronyc tracking 2>/dev/null || echo "")

if [ -z "$TRACKING" ]; then
    log_fail "Cannot read chrony tracking data"
else
    # Extract stratum
    STRATUM=$(echo "$TRACKING" | grep "Stratum" | awk '{print $3}')
    if [ -n "$STRATUM" ]; then
        if [ "$STRATUM" -le 4 ]; then
            log_pass "Stratum is $STRATUM (acceptable: <= 4)"
        elif [ "$STRATUM" -le 6 ]; then
            log_warn "Stratum is $STRATUM (recommended: <= 4)"
        else
            log_fail "Stratum is $STRATUM (too high, expected <= 4)"
        fi
    fi

    # Extract system time offset
    OFFSET_LINE=$(echo "$TRACKING" | grep "System time")
    if [ -n "$OFFSET_LINE" ]; then
        OFFSET_SECS=$(echo "$OFFSET_LINE" | awk '{print $4}')
        OFFSET_MS=$(echo "$OFFSET_SECS * 1000" | bc -l 2>/dev/null || echo "0")
        
        # Check if offset is within bounds
        if (( $(echo "$OFFSET_MS < 1" | bc -l) )); then
            log_pass "System offset is ${OFFSET_MS}ms (target: < 1ms)"
        elif (( $(echo "$OFFSET_MS < 5" | bc -l) )); then
            log_warn "System offset is ${OFFSET_MS}ms (target: < 1ms, acceptable: < 5ms)"
        else
            log_fail "System offset is ${OFFSET_MS}ms (too high, expected < 1ms)"
        fi
    fi

    # Extract root delay
    ROOT_DELAY_LINE=$(echo "$TRACKING" | grep "Root delay")
    if [ -n "$ROOT_DELAY_LINE" ]; then
        ROOT_DELAY_SECS=$(echo "$ROOT_DELAY_LINE" | awk '{print $4}')
        ROOT_DELAY_MS=$(echo "$ROOT_DELAY_SECS * 1000" | bc -l 2>/dev/null || echo "0")
        
        if (( $(echo "$ROOT_DELAY_MS < 50" | bc -l) )); then
            log_pass "Root delay is ${ROOT_DELAY_MS}ms (excellent)"
        elif (( $(echo "$ROOT_DELAY_MS < 100" | bc -l) )); then
            log_pass "Root delay is ${ROOT_DELAY_MS}ms (good)"
        elif (( $(echo "$ROOT_DELAY_MS < 200" | bc -l) )); then
            log_warn "Root delay is ${ROOT_DELAY_MS}ms (consider closer NTP servers)"
        else
            log_fail "Root delay is ${ROOT_DELAY_MS}ms (too high, use closer servers)"
        fi
    fi

    # Extract leap status
    LEAP_STATUS=$(echo "$TRACKING" | grep "Leap status" | awk '{print $4}')
    if [ "$LEAP_STATUS" == "Normal" ]; then
        log_pass "Leap status is Normal"
    elif [ -n "$LEAP_STATUS" ]; then
        log_warn "Leap status is '$LEAP_STATUS' (may indicate pending leap second)"
    fi
fi

# =============================================================================
# Check 3: NTP sources
# =============================================================================
echo ""
echo "=== NTP Sources ==="

SOURCES=$(chronyc sources 2>/dev/null || echo "")

if [ -n "$SOURCES" ]; then
    # Count reachable sources
    REACHABLE=$(echo "$SOURCES" | grep -c "^\^" || echo "0")
    SELECTED=$(echo "$SOURCES" | grep -c "^\^\*" || echo "0")
    
    if [ "$SELECTED" -ge 1 ]; then
        log_pass "Have $SELECTED selected source(s)"
    else
        log_fail "No selected NTP source"
    fi
    
    if [ "$REACHABLE" -ge 3 ]; then
        log_pass "$REACHABLE reachable sources (good redundancy)"
    elif [ "$REACHABLE" -ge 2 ]; then
        log_warn "$REACHABLE reachable sources (recommend >= 3)"
    else
        log_fail "Only $REACHABLE reachable source(s) (need >= 2)"
    fi
    
    if [ "$VERBOSE" == "--verbose" ]; then
        echo ""
        chronyc sources -v
    fi
fi

# =============================================================================
# Check 4: Source statistics
# =============================================================================
echo ""
echo "=== Source Statistics ==="

SOURCESTATS=$(chronyc sourcestats 2>/dev/null || echo "")

if [ -n "$SOURCESTATS" ] && [ "$VERBOSE" == "--verbose" ]; then
    chronyc sourcestats
fi

# Check for high jitter sources
if [ -n "$SOURCESTATS" ]; then
    HIGH_JITTER=$(echo "$SOURCESTATS" | awk 'NR>2 && $6 > 0.005 {print $1}' | head -3)
    if [ -n "$HIGH_JITTER" ]; then
        log_warn "Some sources have high jitter (>5ms): $HIGH_JITTER"
    else
        log_pass "All sources have acceptable jitter"
    fi
fi

# =============================================================================
# Check 5: System clock stability
# =============================================================================
echo ""
echo "=== Clock Stability ==="

# Check for recent clock steps
CHRONY_LOG="/var/log/chrony/tracking.log"
if [ -f "$CHRONY_LOG" ]; then
    RECENT_STEPS=$(grep -c "System clock was stepped" "$CHRONY_LOG" 2>/dev/null || echo "0")
    if [ "$RECENT_STEPS" -eq 0 ]; then
        log_pass "No clock steps recorded in tracking log"
    else
        log_warn "$RECENT_STEPS clock step(s) found in tracking log"
    fi
else
    log_info "Tracking log not found (may need logchange directive in chrony.conf)"
fi

# Check kernel time parameters
if [ -f /proc/sys/kernel/ntp_pll_min_ppm ]; then
    log_info "Kernel NTP PLL available"
fi

# =============================================================================
# Check 6: Configuration recommendations
# =============================================================================
echo ""
echo "=== Configuration Recommendations ==="

CHRONY_CONF="/etc/chrony/chrony.conf"
if [ ! -f "$CHRONY_CONF" ]; then
    CHRONY_CONF="/etc/chrony.conf"
fi

if [ -f "$CHRONY_CONF" ]; then
    # Check for aggressive polling
    if grep -q "minpoll 0\|minpoll 1" "$CHRONY_CONF" 2>/dev/null; then
        log_pass "Aggressive polling configured (minpoll 0-1)"
    else
        log_warn "Consider adding 'minpoll 0' for sub-ms accuracy"
    fi
    
    # Check for makestep
    if grep -q "makestep" "$CHRONY_CONF" 2>/dev/null; then
        log_pass "makestep configured for initial sync"
    else
        log_warn "Consider adding 'makestep 1.0 3' for initial sync"
    fi
    
    # Check for driftfile
    if grep -q "driftfile" "$CHRONY_CONF" 2>/dev/null; then
        log_pass "driftfile configured"
    else
        log_warn "Consider adding 'driftfile /var/lib/chrony/drift'"
    fi
    
    # Check for rtcsync
    if grep -q "rtcsync" "$CHRONY_CONF" 2>/dev/null; then
        log_pass "rtcsync configured"
    else
        log_info "Consider adding 'rtcsync' for hardware clock sync"
    fi
else
    log_warn "Cannot find chrony.conf to check configuration"
fi

# =============================================================================
# Check 7: AWS-specific (if applicable)
# =============================================================================
echo ""
echo "=== Cloud Provider Checks ==="

# Check if running on AWS
if curl -s --connect-timeout 1 http://169.254.169.254/latest/meta-data/ &>/dev/null; then
    log_info "Detected AWS environment"
    
    # Check if using Amazon Time Sync
    if chronyc sources 2>/dev/null | grep -q "169.254.169.123"; then
        log_pass "Using Amazon Time Sync Service (recommended)"
    else
        log_warn "Not using Amazon Time Sync (169.254.169.123)"
        log_info "Add to chrony.conf: server 169.254.169.123 prefer iburst"
    fi
else
    log_info "Not running on AWS (or metadata service unreachable)"
fi

# =============================================================================
# Summary
# =============================================================================
echo ""
echo "=============================================================================
echo "=== Summary ==="

if [ "$CRITICAL" -gt 0 ]; then
    echo -e "${RED}FAILED${NC}: $CRITICAL critical issue(s), $WARNINGS warning(s)"
    echo "Fix critical issues before production deployment."
    exit 1
elif [ "$WARNINGS" -gt 0 ]; then
    echo -e "${YELLOW}WARNING${NC}: $WARNINGS warning(s), no critical issues"
    echo "Consider addressing warnings for optimal performance."
    exit 2
else
    echo -e "${GREEN}PASSED${NC}: All checks passed"
    echo "Time synchronization is configured correctly for HFT latency measurement."
    exit 0
fi
