#!/bin/bash

echo "🧪 BetterBot Phase 8: Testing & Quality Assurance"
echo "=================================================="
echo ""

# Step 1: Build
echo "Step 1: Building BetterBot..."
cd rust-backend
cargo build --release 2>&1 | tail -5
if [ $? -eq 0 ]; then
  echo "✅ Build successful"
else
  echo "❌ Build failed"
  exit 1
fi
echo ""

# Step 2: Start server in background
echo "Step 2: Starting server..."
cd ..
cargo run --manifest-path=rust-backend/Cargo.toml > /tmp/betterbot.log 2>&1 &
SERVER_PID=$!
echo "Server PID: $SERVER_PID"

# Wait for server to start
echo "Waiting for server to be ready..."
for i in {1..30}; do
  if curl -s http://localhost:3000/health > /dev/null 2>&1; then
    echo "✅ Server is ready"
    break
  fi
  if [ $i -eq 30 ]; then
    echo "❌ Server failed to start"
    kill $SERVER_PID 2>/dev/null
    exit 1
  fi
  sleep 1
done
echo ""

# Step 3: Run integration tests
echo "Step 3: Running integration tests..."
./scripts/integration_test.sh
TEST_RESULT=$?
echo ""

# Step 4: Cleanup
echo "Step 4: Stopping server..."
kill $SERVER_PID 2>/dev/null
wait $SERVER_PID 2>/dev/null
echo "✅ Server stopped"
echo ""

# Summary
if [ $TEST_RESULT -eq 0 ]; then
  echo "🎉 ALL TESTS PASSED!"
  echo "BetterBot is production-ready"
  exit 0
else
  echo "❌ TESTS FAILED"
  exit 1
fi
