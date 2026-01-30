#!/bin/bash
# Start BetterBot backend with orderbook recording for backtesting
# This should run continuously for 31 days (Jan 26 - Feb 26, 2026)
#
# Usage: ./start_recording.sh
# Stop:  kill $(cat backend.pid)
# Logs:  tail -f recording.log

set -e

cd "$(dirname "$0")"

# Check if already running
if [ -f backend.pid ] && kill -0 $(cat backend.pid) 2>/dev/null; then
    echo "Backend already running (PID: $(cat backend.pid))"
    echo "To restart: kill $(cat backend.pid) && ./start_recording.sh"
    exit 1
fi

# Build release binary
echo "Building release binary..."
cargo build --release 2>&1 | tail -5

# Start backend with nohup
echo "Starting backend with recording enabled..."
nohup ./target/release/betterbot > recording.log 2>&1 &
echo $! > backend.pid

echo ""
echo "=========================================="
echo "RECORDING STARTED"
echo "=========================================="
echo "PID:      $(cat backend.pid)"
echo "Log:      $(pwd)/recording.log"
echo "Database: $(pwd)/polymarket_recorded.db"
echo ""
echo "Recording targets:"
echo "  - L2 orderbook snapshots"
echo "  - L2 price deltas (price_change messages)"
echo "  - Trade prints"
echo ""
echo "Monitor: tail -f recording.log"
echo "Stop:    kill $(cat backend.pid)"
echo "=========================================="
