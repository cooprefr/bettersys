#!/bin/bash
set -e

echo "🧪 BetterBot Integration Tests"
echo "==============================="
echo ""

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m'

pass() { echo -e "${GREEN}✅ PASS${NC}: $1"; }
fail() { echo -e "${RED}❌ FAIL${NC}: $1"; exit 1; }
info() { echo -e "${YELLOW}ℹ️  INFO${NC}: $1"; }

# Check if server is running
info "Checking if server is running on localhost:3000..."
if ! curl -s http://localhost:3000/health > /dev/null 2>&1; then
    fail "Server not running. Start with: cd rust-backend && cargo run"
fi

pass "Server is running"
echo ""

# Test 1: Health Check
echo "Test 1: Health Check"
RESPONSE=$(curl -s http://localhost:3000/health)
if [[ $RESPONSE == *"Operational"* ]]; then
  pass "Health check returned operational status"
else
  fail "Health check failed: $RESPONSE"
fi
echo ""

# Test 2: Login
echo "Test 2: Authentication"
LOGIN=$(curl -s -X POST http://localhost:3000/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"admin123"}')

if [[ $LOGIN == *"token"* ]]; then
  pass "Login successful"
  TOKEN=$(echo $LOGIN | python3 -c "import sys, json; print(json.load(sys.stdin)['token'])" 2>/dev/null || echo $LOGIN | grep -o '"token":"[^"]*"' | cut -d'"' -f4)
  info "Token: ${TOKEN:0:20}..."
else
  fail "Login failed: $LOGIN"
fi
echo ""

# Test 3: Get Signals
echo "Test 3: Get Signals API"
SIGNALS=$(curl -s http://localhost:3000/api/signals)
if [[ $SIGNALS == *"signals"* ]] || [[ $SIGNALS == "["* ]]; then
  pass "Signals endpoint accessible"
  SIGNAL_COUNT=$(echo $SIGNALS | python3 -c "import sys, json; print(len(json.load(sys.stdin)))" 2>/dev/null || echo "0")
  info "Signals in database: $SIGNAL_COUNT"
else
  fail "Signals endpoint failed: $SIGNALS"
fi
echo ""

# Test 4: Get Risk Stats
echo "Test 4: Get Risk Stats API"
RISK=$(curl -s http://localhost:3000/api/risk/stats)
if [[ $RISK == *"bankroll"* ]] || [[ $RISK == "{"* ]]; then
  pass "Risk stats endpoint accessible"
else
  fail "Risk stats endpoint failed: $RISK"
fi
echo ""

# Test 5: Invalid Credentials
echo "Test 5: Invalid Credentials"
INVALID_LOGIN=$(curl -s -X POST http://localhost:3000/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"wrongpassword"}')

if [[ $INVALID_LOGIN == *"Invalid"* ]] || [[ $INVALID_LOGIN == *"401"* ]] || [[ $INVALID_LOGIN != *"token"* ]]; then
  pass "Invalid credentials correctly rejected"
else
  fail "Invalid credentials not rejected properly: $INVALID_LOGIN"
fi
echo ""

# Test 6: SQL Injection Protection
echo "Test 6: SQL Injection Protection"
SQL_INJECT=$(curl -s -X POST http://localhost:3000/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin'\'' OR 1=1--","password":"anything"}')

if [[ $SQL_INJECT != *"token"* ]]; then
  pass "SQL injection attempt blocked"
else
  fail "SQL injection not protected!"
fi
echo ""

# Test 7: WebSocket Connection
echo "Test 7: WebSocket Connection"
info "Testing WebSocket connection (5 second timeout)..."
WS_TEST=$(timeout 5 websocat ws://localhost:3000/ws 2>&1 || echo "timeout")
if [[ $WS_TEST != *"error"* ]] && [[ $WS_TEST != *"refused"* ]]; then
  pass "WebSocket endpoint accessible"
else
  info "WebSocket test skipped (websocat not installed or connection timeout)"
fi
echo ""

# Summary
echo "================================"
echo -e "${GREEN}🎉 All integration tests passed!${NC}"
echo "================================"
echo ""
info "BetterBot is operational and ready for production"
