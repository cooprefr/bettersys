#!/bin/bash

# BetterBot Launch Script
# Starts both Rust backend and React frontend

set -e

# Load nvm if available
export NVM_DIR="$HOME/.nvm"
[ -s "$NVM_DIR/nvm.sh" ] && \. "$NVM_DIR/nvm.sh"  # This loads nvm

echo "╔═══════════════════════════════════════════════════════════════╗"
echo "║                                                               ║"
echo "║   ██████╗ ███████╗████████╗████████╗███████╗██████╗          ║"
echo "║   ██╔══██╗██╔════╝╚══██╔══╝╚══██╔══╝██╔════╝██╔══██╗         ║"
echo "║   ██████╔╝█████╗     ██║      ██║   █████╗  ██████╔╝         ║"
echo "║   ██╔══██╗██╔══╝     ██║      ██║   ██╔══╝  ██╔══██╗         ║"
echo "║   ██████╔╝███████╗   ██║      ██║   ███████╗██║  ██║         ║"
echo "║   ╚═════╝ ╚══════╝   ╚═╝      ╚═╝   ╚══════╝╚═╝  ╚═╝         ║"
echo "║                                                               ║"
echo "║              ALPHA SIGNAL FEED // v2.0                       ║"
echo "║                                                               ║"
echo "╚═══════════════════════════════════════════════════════════════╝"
echo ""

# Check if .env exists
if [ ! -f "rust-backend/.env" ]; then
    echo "⚠️  Warning: rust-backend/.env not found!"
    if [ -f "rust-backend/src/.env.example" ]; then
        echo "Creating .env from src/.env.example..."
        cp rust-backend/src/.env.example rust-backend/.env
    elif [ -f "rust-backend/.env.example" ]; then
        echo "Creating .env from .env.example..."
        cp rust-backend/.env.example rust-backend/.env
    else
        echo "❌ .env.example not found. Skipping .env creation."
    fi
    echo "✓ Created .env file"
    echo ""
    echo "⚡ IMPORTANT: Edit rust-backend/.env and add your HASHDIVE_API_KEY"
    echo ""
    # read -p "Press Enter to continue..." # Removed for automation
fi

# Check if node_modules exists
if [ ! -d "frontend/node_modules" ]; then
    echo "📦 Installing frontend dependencies..."
    cd frontend
    npm install
    cd ..
    echo "✓ Dependencies installed"
    echo ""
fi

# Kill any existing betterbot processes
echo "🧹 Cleaning up old processes..."
pkill -f "target/release/betterbot" 2>/dev/null || true
lsof -ti:3000 | xargs kill -9 2>/dev/null || true
lsof -ti:5173 | xargs kill -9 2>/dev/null || true
sleep 1
echo "✓ Cleanup complete"
echo ""

# Function to cleanup on exit
cleanup() {
    echo ""
    echo "🛑 Shutting down BetterBot..."
    kill $BACKEND_PID 2>/dev/null || true
    kill $FRONTEND_PID 2>/dev/null || true
    # cargo/npm can spawn child processes; ensure ports are freed
    lsof -ti:3000 | xargs kill -9 2>/dev/null || true
    lsof -ti:5173 | xargs kill -9 2>/dev/null || true
    echo "✓ Shutdown complete"
    echo ""
    echo "[SEE YOU SPACE COWBOY...]"
    exit 0
}

trap cleanup INT TERM

# Start Rust backend
echo "🚀 Starting Rust backend..."
cd rust-backend
cargo build --release 2>&1 | grep -E "(Compiling|Finished)" &
sleep 2
cargo run --release --bin betterbot > /tmp/betterbot-backend.log 2>&1 &
BACKEND_PID=$!
cd ..

echo "⏳ Waiting for backend to start..."
sleep 5

# Check if backend is running
if ! curl -s http://localhost:3000/health > /dev/null 2>&1; then
    echo "⚠️ Backend health check failed (might still be starting). Check logs:"
    tail -n 5 /tmp/betterbot-backend.log
else
    echo "✓ Backend started (PID: $BACKEND_PID)"
    echo "   → http://localhost:3000"
fi
echo ""

# Start React frontend
echo "🎨 Starting React frontend (Nostromo Interface)..."
cd frontend
npm run dev -- --host 0.0.0.0 --port 5173 --strictPort > /tmp/betterbot-frontend.log 2>&1 &
FRONTEND_PID=$!
cd ..

echo "⏳ Waiting for frontend to start..."
sleep 3

if ! curl -sSf http://localhost:5173/ > /dev/null 2>&1; then
    echo "⚠️ Frontend check failed. Check logs:"
    tail -n 20 /tmp/betterbot-frontend.log
else
    echo "✓ Frontend started (PID: $FRONTEND_PID)"
    echo "   → http://localhost:5173"
fi
echo ""

echo "╔═══════════════════════════════════════════════════════════════╗"
echo "║                                                               ║"
echo "║           🌌 PROJECT NOSTROMO: LAVENDER HORIZON 🌌           ║"
echo "║                                                               ║"
echo "║   Frontend:  http://localhost:5173                           ║"
echo "║   Backend:   http://localhost:3000                           ║"
echo "║   API Docs:  http://localhost:3000/health                    ║"
echo "║                                                               ║"
echo "║   Press CTRL+C to stop all services                          ║"
echo "║                                                               ║"
echo "╚═══════════════════════════════════════════════════════════════╝"
echo ""

# Open browser (macOS)
if command -v open &> /dev/null; then
    echo "🌐 Opening browser..."
    sleep 2
    open http://localhost:5173
fi

# Keep script running
echo "📊 Monitoring logs (press CTRL+C to stop)..."
echo ""
tail -f /tmp/betterbot-backend.log /tmp/betterbot-frontend.log
