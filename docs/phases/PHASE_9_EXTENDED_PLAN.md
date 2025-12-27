# Phase 9 Extended: Production Deployment + Trading Terminal

**Status:** 🚀 READY TO BUILD  
**Priority:** CRITICAL  
**Estimated Time:** 6-8 hours  
**Date:** November 16, 2025  

---

## Mission Statement

**Deploy BetterBot to production with a world-class brutalist synthwave trading terminal that provides real-time signal intelligence with sub-5ms latency.**

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│                     BETTERBOT SYSTEM                        │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  ┌──────────────┐     WebSocket      ┌──────────────┐     │
│  │   Frontend   │ ◄─────────────────► │   Backend    │     │
│  │  (Terminal)  │                     │   (Rust)     │     │
│  └──────────────┘     REST API        └──────────────┘     │
│        │                                      │             │
│        │                                      │             │
│        ▼                                      ▼             │
│  ┌──────────────┐                     ┌──────────────┐     │
│  │  Static CDN  │                     │   SQLite     │     │
│  │ (Cloudflare) │                     │  Databases   │     │
│  └──────────────┘                     └──────────────┘     │
│                                              │             │
│                                              ▼             │
│                                       ┌──────────────┐     │
│                                       │ External APIs│     │
│                                       │ (DomeAPI,    │     │
│                                       │  Polymarket) │     │
│                                       └──────────────┘     │
└─────────────────────────────────────────────────────────────┘
```

---

## Part A: Production Deployment (Backend)

### 1. Environment Setup

**File:** `.env.production`
```bash
# Server
RUST_LOG=info,betterbot_backend=debug
HOST=0.0.0.0
PORT=3000

# Database
DB_PATH=./data/betterbot_signals.db
AUTH_DB_PATH=./data/betterbot_auth.db

# Authentication (CRITICAL: Change in production!)
JWT_SECRET=CHANGE_THIS_TO_A_SECURE_64_CHAR_SECRET_IN_PRODUCTION_ABCDEF123456789
JWT_EXPIRATION_HOURS=24

# Risk Management
INITIAL_BANKROLL=10000
KELLY_FRACTION=0.25

# API Keys (Optional - for real data)
DOME_API_KEY=your_dome_api_key_here
HASHDIVE_API_KEY=your_hashdive_api_key_here

# CORS (Production domains)
ALLOWED_ORIGINS=https://betterbot.ai,http://localhost:5173
```

### 2. Systemd Service (Linux)

**File:** `/etc/systemd/system/betterbot.service`
```ini
[Unit]
Description=BetterBot Trading Engine
After=network.target

[Service]
Type=simple
User=betterbot
WorkingDirectory=/opt/betterbot
ExecStart=/opt/betterbot/target/release/betterbot
Restart=always
RestartSec=10
StandardOutput=journal
StandardError=journal
Environment="RUST_LOG=info"

[Install]
WantedBy=multi-user.target
```

### 3. Nginx Reverse Proxy

**File:** `/etc/nginx/sites-available/betterbot`
```nginx
upstream betterbot_backend {
    server 127.0.0.1:3000;
}

server {
    listen 80;
    listen [::]:80;
    server_name api.betterbot.ai;

    # Redirect to HTTPS
    return 301 https://$server_name$request_uri;
}

server {
    listen 443 ssl http2;
    listen [::]:443 ssl http2;
    server_name api.betterbot.ai;

    # SSL Configuration (Let's Encrypt)
    ssl_certificate /etc/letsencrypt/live/api.betterbot.ai/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/api.betterbot.ai/privkey.pem;
    ssl_protocols TLSv1.2 TLSv1.3;
    ssl_ciphers HIGH:!aNULL:!MD5;

    # Security Headers
    add_header X-Frame-Options "SAMEORIGIN" always;
    add_header X-Content-Type-Options "nosniff" always;
    add_header X-XSS-Protection "1; mode=block" always;
    add_header Referrer-Policy "no-referrer-when-downgrade" always;

    # WebSocket Support
    location /ws {
        proxy_pass http://betterbot_backend;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_read_timeout 86400;
    }

    # API Endpoints
    location /api {
        proxy_pass http://betterbot_backend;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }

    # Health Check
    location /health {
        proxy_pass http://betterbot_backend;
        access_log off;
    }
}
```

---

## Part B: Trading Terminal Frontend

### Design Philosophy

**Aesthetic:** Brutalist Synthwave × 80s Anime × CRT Terminal  
**Colors:**
- Background: `#020208` (Void Black)
- Terminal: `#050A0E` (Deep Space)
- Primary: `#6366f1` (Indigo Neon)
- Accent: `#F97316` (Orange Alert)
- Success: `#10b981` (Matrix Green)
- Warning: `#fbbf24` (Amber Pulse)
- Error: `#ef4444` (Critical Red)

**Fonts:**
- Primary: `Space Mono` (monospace, tech feel)
- Terminal: `VT323` (retro terminal)
- Headers: `Orbitron` (futuristic)

**Effects:**
- CRT scanlines (animated)
- Phosphor glow on hover
- Glitch text animations
- Data stream particles
- 3D cube rotations
- Signal pulse waves

### Tech Stack

```
Frontend:
- Vite + React + TypeScript
- TailwindCSS + Custom Animations
- Chart.js (real-time graphs)
- Three.js (3D visualizations)
- WebSocket (live data feed)
- Zustand (state management)

Build:
- Vite (lightning fast HMR)
- ESBuild (sub-second builds)
- PostCSS (CSS optimization)
```

### Project Structure

```
frontend/
├── public/
│   ├── favicon.ico
│   └── fonts/
│       ├── SpaceMono.woff2
│       ├── VT323.woff2
│       └── Orbitron.woff2
├── src/
│   ├── components/
│   │   ├── terminal/
│   │   │   ├── SignalFeed.tsx          # Live signal stream
│   │   │   ├── SignalCard.tsx          # Individual signal display
│   │   │   ├── TerminalHeader.tsx      # Top bar with stats
│   │   │   ├── MarketGrid.tsx          # Market overview grid
│   │   │   └── SignalChart.tsx         # Confidence/time graph
│   │   ├── effects/
│   │   │   ├── CRTOverlay.tsx          # Scanlines & CRT effect
│   │   │   ├── GlitchText.tsx          # Glitch animation
│   │   │   ├── ParticleField.tsx       # 3D background particles
│   │   │   └── ScanLine.tsx            # Animated scan line
│   │   ├── auth/
│   │   │   ├── LoginScreen.tsx         # Cyberpunk login
│   │   │   └── AuthGuard.tsx           # Route protection
│   │   └── layout/
│   │       ├── AppShell.tsx            # Main layout wrapper
│   │       ├── Sidebar.tsx             # Navigation
│   │       └── StatusBar.tsx           # Bottom status bar
│   ├── hooks/
│   │   ├── useWebSocket.ts             # WebSocket connection
│   │   ├── useSignals.ts               # Signal data management
│   │   ├── useAuth.ts                  # Authentication
│   │   └── useAnimations.ts            # Animation utilities
│   ├── services/
│   │   ├── api.ts                      # REST API client
│   │   ├── websocket.ts                # WebSocket client
│   │   └── auth.ts                     # Auth service
│   ├── stores/
│   │   ├── signalStore.ts              # Signal state
│   │   ├── authStore.ts                # Auth state
│   │   └── uiStore.ts                  # UI preferences
│   ├── types/
│   │   ├── signal.ts                   # Signal types
│   │   ├── auth.ts                     # Auth types
│   │   └── api.ts                      # API types
│   ├── styles/
│   │   ├── globals.css                 # Global styles
│   │   ├── animations.css              # Custom animations
│   │   └── terminal.css                # Terminal effects
│   ├── utils/
│   │   ├── formatters.ts               # Data formatting
│   │   ├── colors.ts                   # Color utilities
│   │   └── sound.ts                    # Sound effects
│   ├── App.tsx                         # Root component
│   ├── main.tsx                        # Entry point
│   └── vite-env.d.ts                   # Vite types
├── index.html
├── package.json
├── tsconfig.json
├── vite.config.ts
├── tailwind.config.js
└── postcss.config.js
```

---

## Part C: Trading Terminal Features

### 1. Login Screen (Cyberpunk Auth)

```
┌─────────────────────────────────────────────────────────────┐
│                                                             │
│                    ╔══════════════════╗                     │
│                    ║   BETTERBOT      ║                     │
│                    ║   $BETTER        ║                     │
│                    ║   TERMINAL v1.0  ║                     │
│                    ╚══════════════════╝                     │
│                                                             │
│              ┌─────────────────────────────┐               │
│              │ USERNAME: _________________ │               │
│              └─────────────────────────────┘               │
│                                                             │
│              ┌─────────────────────────────┐               │
│              │ PASSWORD: ***************** │               │
│              └─────────────────────────────┘               │
│                                                             │
│                  [ AUTHENTICATE > ]                         │
│                                                             │
│            STATUS: AWAITING CREDENTIALS                     │
│            LATENCY: <5ms | UPTIME: 99.99%                  │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

### 2. Main Terminal Dashboard

```
┌─────────────────────────────────────────────────────────────────────────┐
│ BETTERBOT [$BETTER] │ SIGNALS: 127 │ UPTIME: 14h 32m │ LATENCY: 3.2ms │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  ┌───────────────┐  ┌───────────────┐  ┌───────────────┐             │
│  │ LIVE SIGNALS  │  │ HIGH CONF     │  │ ARBITRAGE     │             │
│  │      127      │  │      42       │  │      8        │             │
│  │   ▲ +15/min   │  │   ≥80%        │  │   Active      │             │
│  └───────────────┘  └───────────────┘  └───────────────┘             │
│                                                                         │
│  ┌─────────────────────────────────────────────────────────────────┐  │
│  │ REAL-TIME SIGNAL STREAM                                         │  │
│  ├─────────────────────────────────────────────────────────────────┤  │
│  │ [18:42:33] 🐋 WHALE ENTRY                                        │  │
│  │   Market: will-trump-win-2024                                    │  │
│  │   Confidence: 92.4% | Position: $45,200 | Side: YES             │  │
│  │   Signal: Elite wallet 0x7a3b... entered with conviction        │  │
│  │   ───────────────────────────────────────────────────────────    │  │
│  │ [18:42:29] 💎 ARBITRAGE DETECTED                                 │  │
│  │   Market: ethereum-above-3000                                    │  │
│  │   Confidence: 87.1% | Spread: 3.2% | Expected: +2.8%            │  │
│  │   Polymarket: $0.62 ← → Kalshi: $0.65                          │  │
│  │   ───────────────────────────────────────────────────────────    │  │
│  │ [18:42:15] ⏰ EXPIRY EDGE                                        │  │
│  │   Market: btc-50k-by-eow                                        │  │
│  │   Confidence: 95.2% | Time: 2h 15m | Dominant: YES 78%         │  │
│  │   Action: FOLLOW DOMINANT SIDE (95% win rate historical)        │  │
│  └─────────────────────────────────────────────────────────────────┘  │
│                                                                         │
│  ┌───────────────────────────┐  ┌────────────────────────────────┐   │
│  │ CONFIDENCE DISTRIBUTION   │  │ SIGNAL TYPES (24h)             │   │
│  │                           │  │                                 │   │
│  │    95%+ ██████ 12         │  │ Whale Entry    ████████ 45     │   │
│  │  90-95% ████████████ 30   │  │ Arbitrage      ███ 18          │   │
│  │  80-90% ████████████████  │  │ Expiry Edge    ██ 12           │   │
│  │  <80%   █████ 15          │  │ Tracked Wallet ████ 28         │   │
│  └───────────────────────────┘  │ Volume Spike   ██ 10           │   │
│                                  └────────────────────────────────┘   │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
 [F1:HELP] [F2:FILTER] [F3:MARKETS] [F4:STATS] [ESC:MENU] │ Connected ●
```

### 3. Signal Detail Modal

```
┌─────────────────────────────────────────────────────────────┐
│  SIGNAL DETAIL │ ID: sig_1731708153_abc123                   │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  TYPE:        🐋 WHALE FOLLOWING                            │
│  MARKET:      will-trump-win-2024                          │
│  DETECTED:    2025-11-16 18:42:33 UTC (2 minutes ago)     │
│                                                             │
│  ┌─────────────────────────────────────────────────────┐  │
│  │ CONFIDENCE SCORE                                    │  │
│  │ ████████████████████░░ 92.4%                        │  │
│  │                                                     │  │
│  │ Risk Level: LOW                                     │  │
│  └─────────────────────────────────────────────────────┘  │
│                                                             │
│  SIGNAL DETAILS:                                            │
│  • Wallet: 0x7a3b4f2c...e891 (Elite Tier)                 │
│  • Position Size: $45,200                                  │
│  • Direction: YES                                          │
│  • Entry Price: $0.67                                      │
│  • Historical Win Rate: 87.3% (last 100 trades)           │
│  • Average Hold Time: 4.2 days                            │
│                                                             │
│  MARKET CONTEXT:                                            │
│  • Current Price: $0.67                                    │
│  • 24h Volume: $2.4M                                       │
│  • Liquidity: $450K                                        │
│  • Expiry: 2024-11-05 23:59 UTC                           │
│                                                             │
│  RECOMMENDED ACTION:                                        │
│  ► FOLLOW BUY (Mirror elite wallet position)              │
│                                                             │
│           [ ACKNOWLEDGE ]  [ IGNORE ]  [ ALERT ]           │
└─────────────────────────────────────────────────────────────┘
```

### 4. Market Grid View

```
┌─────────────────────────────────────────────────────────────┐
│  MARKETS OVERVIEW │ Sorted by Signal Activity               │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  MARKET                    SIGNALS  CONF   PRICE   24H     │
│  ────────────────────────  ───────  ─────  ──────  ─────   │
│  will-trump-win-2024          15    92.4%  $0.67   +2.3%  │
│  ethereum-above-3000          12    87.1%  $0.45   -1.2%  │
│  btc-50k-by-eow               8     95.2%  $0.82   +5.1%  │
│  fed-rate-cut-december        6     78.9%  $0.34   +0.8%  │
│  nasdaq-ath-2024              5     84.3%  $0.56   +3.2%  │
│  oil-above-90                 4     76.2%  $0.23   -2.1%  │
│                                                             │
│  [ Filter: ALL ▼ ] [ Sort: SIGNALS ▼ ] [ Refresh: 5s ]   │
└─────────────────────────────────────────────────────────────┘
```

---

## Part D: Implementation Details

### 1. WebSocket Integration

**File:** `frontend/src/services/websocket.ts`
```typescript
import { useEffect, useRef, useState } from 'react';
import { Signal } from '../types/signal';

export const useWebSocket = (url: string) => {
  const ws = useRef<WebSocket | null>(null);
  const [signals, setSignals] = useState<Signal[]>([]);
  const [connected, setConnected] = useState(false);
  const [latency, setLatency] = useState(0);

  useEffect(() => {
    // Connect to WebSocket
    ws.current = new WebSocket(url);
    
    ws.current.onopen = () => {
      setConnected(true);
      console.log('🔌 WebSocket connected');
      
      // Send ping every 30s to measure latency
      setInterval(() => {
        if (ws.current?.readyState === WebSocket.OPEN) {
          const start = Date.now();
          ws.current.send('ping');
          // Measure round-trip time on pong
        }
      }, 30000);
    };
    
    ws.current.onmessage = (event) => {
      try {
        const signal: Signal = JSON.parse(event.data);
        
        // Add to signals array (keep last 100)
        setSignals(prev => [signal, ...prev].slice(0, 100));
        
        // Play sound effect
        playSignalSound(signal.confidence);
        
        // Show notification for high confidence
        if (signal.confidence >= 0.90) {
          showNotification(signal);
        }
      } catch (error) {
        console.error('Failed to parse signal:', error);
      }
    };
    
    ws.current.onerror = (error) => {
      console.error('WebSocket error:', error);
      setConnected(false);
    };
    
    ws.current.onclose = () => {
      setConnected(false);
      // Attempt reconnection after 5s
      setTimeout(() => {
        window.location.reload();
      }, 5000);
    };
    
    return () => {
      ws.current?.close();
    };
  }, [url]);
  
  return { signals, connected, latency };
};
```

### 2. Signal Feed Component

**File:** `frontend/src/components/terminal/SignalFeed.tsx`
```typescript
import React, { useEffect, useRef } from 'react';
import { motion, AnimatePresence } from 'framer-motion';
import { Signal } from '../../types/signal';
import { SignalCard } from './SignalCard';

interface SignalFeedProps {
  signals: Signal[];
}

export const SignalFeed: React.FC<SignalFeedProps> = ({ signals }) => {
  const feedRef = useRef<HTMLDivElement>(null);
  const [autoScroll, setAutoScroll] = useState(true);
  
  useEffect(() => {
    if (autoScroll && feedRef.current) {
      feedRef.current.scrollTop = 0;
    }
  }, [signals, autoScroll]);
  
  return (
    <div className="relative h-full overflow-hidden">
      {/* Header */}
      <div className="brutal-border p-4 flex justify-between items-center">
        <div className="flex items-center gap-4">
          <h2 className="text-2xl font-terminal text-indigo-400">
            REAL-TIME SIGNAL STREAM
          </h2>
          <div className="text-green-400 font-mono">
            ● LIVE ({signals.length})
          </div>
        </div>
        
        <button
          onClick={() => setAutoScroll(!autoScroll)}
          className={`brutal-border px-3 py-1 text-sm ${
            autoScroll ? 'bg-indigo-900/30' : ''
          }`}
        >
          AUTO-SCROLL: {autoScroll ? 'ON' : 'OFF'}
        </button>
      </div>
      
      {/* Feed */}
      <div
        ref={feedRef}
        className="h-[calc(100%-64px)] overflow-y-auto scrollbar-thin scrollbar-track-gray-900 scrollbar-thumb-indigo-600"
      >
        <AnimatePresence initial={false}>
          {signals.map((signal, index) => (
            <motion.div
              key={signal.id}
              initial={{ opacity: 0, x: -50 }}
              animate={{ opacity: 1, x: 0 }}
              exit={{ opacity: 0, x: 50 }}
              transition={{ duration: 0.3, delay: index * 0.05 }}
            >
              <SignalCard signal={signal} />
            </motion.div>
          ))}
        </AnimatePresence>
        
        {signals.length === 0 && (
          <div className="flex items-center justify-center h-full">
            <div className="text-center text-gray-500">
              <div className="text-6xl mb-4">📡</div>
              <div className="text-xl font-terminal">
                SCANNING FOR SIGNALS...
              </div>
              <div className="text-sm mt-2">
                Waiting for market opportunities
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
};
```

### 3. Signal Card Component

**File:** `frontend/src/components/terminal/SignalCard.tsx`
```typescript
import React from 'react';
import { Signal } from '../../types/signal';
import { getSignalIcon, getSignalColor } from '../../utils/signals';

interface SignalCardProps {
  signal: Signal;
}

export const SignalCard: React.FC<SignalCardProps> = ({ signal }) => {
  const icon = getSignalIcon(signal.signal_type);
  const color = getSignalColor(signal.confidence);
  const timeAgo = getTimeAgo(signal.detected_at);
  
  return (
    <div className="brutal-border m-2 p-4 hover:shadow-indigo-500/50 transition-all duration-300">
      {/* Header */}
      <div className="flex items-center justify-between mb-2">
        <div className="flex items-center gap-3">
          <span className="text-2xl">{icon}</span>
          <div>
            <div className="text-sm text-gray-400 font-mono">
              [{signal.detected_at.slice(11, 19)}]
            </div>
            <div className="text-lg font-bold text-indigo-300">
              {getSignalTypeName(signal.signal_type)}
            </div>
          </div>
        </div>
        
        <div className={`text-right ${color}`}>
          <div className="text-2xl font-bold font-mono">
            {(signal.confidence * 100).toFixed(1)}%
          </div>
          <div className="text-xs text-gray-400">
            CONFIDENCE
          </div>
        </div>
      </div>
      
      {/* Market */}
      <div className="mb-3">
        <div className="text-sm text-gray-400">MARKET</div>
        <div className="font-mono text-white">
          {signal.market_slug}
        </div>
      </div>
      
      {/* Details */}
      <div className="text-sm text-gray-300 mb-3">
        {signal.details.market_title}
      </div>
      
      {/* Metrics */}
      <div className="grid grid-cols-3 gap-2 text-xs">
        <div className="brutal-border p-2">
          <div className="text-gray-400">PRICE</div>
          <div className="font-mono text-white">
            ${signal.details.current_price.toFixed(2)}
          </div>
        </div>
        
        <div className="brutal-border p-2">
          <div className="text-gray-400">24H VOL</div>
          <div className="font-mono text-white">
            ${(signal.details.volume_24h / 1000).toFixed(0)}K
          </div>
        </div>
        
        <div className="brutal-border p-2">
          <div className="text-gray-400">ACTION</div>
          <div className="font-mono text-orange-400">
            {signal.details.recommended_action}
          </div>
        </div>
      </div>
      
      {/* Footer */}
      <div className="mt-3 flex justify-between items-center text-xs text-gray-500">
        <div>Source: {signal.source}</div>
        <div>{timeAgo}</div>
      </div>
    </div>
  );
};
```

---

## Part E: Deployment Steps

### Step 1: Backend Deployment (5 steps)

```bash
# 1. Build optimized release
cd rust-backend
cargo build --release

# 2. Create deployment directory
sudo mkdir -p /opt/betterbot
sudo mkdir -p /opt/betterbot/data
sudo cp target/release/betterbot /opt/betterbot/
sudo cp .env.production /opt/betterbot/.env

# 3. Create betterbot user
sudo useradd -r -s /bin/false betterbot
sudo chown -R betterbot:betterbot /opt/betterbot

# 4. Install systemd service
sudo cp ../deploy/betterbot.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable betterbot
sudo systemctl start betterbot

# 5. Check status
sudo systemctl status betterbot
sudo journalctl -u betterbot -f
```

### Step 2: Frontend Deployment (4 steps)

```bash
# 1. Install dependencies
cd frontend
npm install

# 2. Configure API endpoint
echo "VITE_API_URL=https://api.betterbot.ai" > .env.production
echo "VITE_WS_URL=wss://api.betterbot.ai/ws" >> .env.production

# 3. Build for production
npm run build

# 4. Deploy to CDN (Cloudflare Pages, Vercel, or Netlify)
# Option A: Cloudflare Pages
wrangler pages publish dist

# Option B: Vercel
vercel --prod

# Option C: Static hosting
sudo cp -r dist/* /var/www/betterbot/
```

### Step 3: SSL/TLS Setup (Let's Encrypt)

```bash
# Install certbot
sudo apt install certbot python3-certbot-nginx

# Obtain certificate
sudo certbot --nginx -d api.betterbot.ai

# Auto-renewal (already configured)
sudo certbot renew --dry-run
```

### Step 4: Monitoring Setup

```bash
# Install monitoring tools
sudo apt install prometheus grafana

# Configure Prometheus to scrape /metrics endpoint
# (Add metrics endpoint to Rust backend)

# Access Grafana
# http://localhost:3000
# Default: admin/admin
```

---

## Part F: Local Testing Guide

### Prerequisites

```bash
# Install Rust (if not already)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install Node.js 18+
curl -fsSL https://deb.nodesource.com/setup_18.x | sudo -E bash -
sudo apt install -y nodejs

# Install basic tools
sudo apt install -y build-essential pkg-config libssl-dev
```

### Local Development Setup

```bash
# 1. Clone and setup backend
cd /Users/aryaman/betterbot/rust-backend

# 2. Create local .env
cat > .env << EOF
RUST_LOG=debug
DB_PATH=./betterbot_signals.db
AUTH_DB_PATH=./betterbot_auth.db
JWT_SECRET=dev-secret-key-for-local-testing-only-change-in-prod
INITIAL_BANKROLL=10000
KELLY_FRACTION=0.25
EOF

# 3. Run backend
cargo run

# In another terminal:
# 4. Setup frontend
cd ../frontend
npm install

# 5. Create frontend .env
cat > .env.development << EOF
VITE_API_URL=http://localhost:3000
VITE_WS_URL=ws://localhost:3000/ws
EOF

# 6. Run frontend dev server
npm run dev

# Frontend will be available at: http://localhost:5173
# Backend API at: http://localhost:3000
```

### Testing Checklist

```bash
# 1. Test backend health
curl http://localhost:3000/health

# 2. Test login
curl -X POST http://localhost:3000/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"admin123"}'

# 3. Test signals endpoint
curl http://localhost:3000/api/signals

# 4. Test WebSocket (using wscat)
npm install -g wscat
wscat -c ws://localhost:3000/ws

# 5. Open browser
open http://localhost:5173

# 6. Check browser console for:
# - WebSocket connection: Connected ●
# - No errors
# - Signals appearing in feed
```

---

## Part G: Performance Targets

| Metric | Target | Why |
|--------|--------|-----|
| **Page Load** | <1s | First contentful paint |
| **WebSocket Latency** | <5ms | Real-time feel |
| **Signal Display** | <10ms | Instant feedback |
| **API Response** | <50ms | Snappy interactions |
| **Bundle Size** | <500KB | Fast downloads |
| **Memory Usage** | <100MB | Efficient |

---

## Timeline

| Task | Duration | Owner |
|------|----------|-------|
| **Backend Deployment** | 2h | DevOps |
| **Frontend Development** | 4-5h | Frontend Dev |
| **Integration Testing** | 1h | QA |
| **Production Deploy** | 1h | DevOps |
| **Monitoring Setup** | 30m | DevOps |
| **Total** | **8-9h** | Team |

---

## Success Criteria

✅ Backend deployed and stable  
✅ Frontend deployed and accessible  
✅ WebSocket connection working  
✅ Real-time signals displaying  
✅ Authentication functional  
✅ <5ms WebSocket latency  
✅ Zero console errors  
✅ Mobile responsive  
✅ SSL/TLS configured  
✅ Monitoring active  

---

*"Ship fast, iterate faster. The terminal awaits."*

**Ready to build the most badass trading terminal ever created? Let's go! 🚀**
