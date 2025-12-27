# 🤖 BetterBot v2.0

**Alpha signal detection for prediction markets** • Built with Rust + React • Token-gated for $BETTER holders

```
     ___  ___ _____ _____ ___ ___  ___  ___ _____ 
    | _ )| __|_   _|_   _| __| _ \| _ )/ _ \_   _|
    | _ \| _|  | |   | | | _||   /| _ \ (_) || |  
    |___/|___| |_|   |_| |___|_|_\|___/\___/ |_|  

    ALPHA SIGNAL FEED // v2.0
```

---

## 🚀 Quick Start

### One-Command Launch
```bash
./start.sh
```

This will:
1. Build the Rust backend
2. Install frontend dependencies
3. Start both services
4. Open your browser automatically

**Default URLs:**
- Frontend: `http://localhost:3000`
- Backend API: `http://localhost:8080`

### Manual Launch

#### Terminal 1: Backend
```bash
cd rust-backend
cargo run --release
```

#### Terminal 2: Frontend
```bash
cd terminal-ui
npm install
npm run dev
```

---

## 📊 Features

### 6 Advanced Signal Types

| # | Signal | Description | Source |
|---|--------|-------------|--------|
| 1 | **Whale Following** | Individual whale trades >$10k | Hashdive |
| 2 | **Volume Spike** | 10x+ volume increase (24hr) | Polymarket |
| 3 | **Spread Analysis** | Wide spreads (15%+ deviation) | Polymarket |
| 4 | **Price Deviation** | Yes+No ≠ $1.00 arbitrage | Polymarket |
| 5 | **Whale Cluster** | 3+ whales same direction (1hr) | Hashdive |
| 6 | **Market Expiry Edge** | 95% accuracy within 4hrs | Polymarket |

### Terminal UI Features

- ✨ **Cowboy Bebop Aesthetic** - Brutalist, retro-futuristic design
- 🖥️ **AMOLED Black** - Pure #000000 background
- 📡 **Real-time Updates** - 5-second polling
- 📊 **Live Stats Dashboard** - Total signals, 24hr count, avg confidence
- 🎨 **CRT Effects** - Scanlines, terminal flicker, blinking cursor
- 📱 **Responsive** - Works on desktop and mobile

---

## 🛠️ Tech Stack

### Backend (Rust)
- **Tokio** - Async runtime
- **Axum** - REST API framework
- **SQLite** - Signal storage
- **Reqwest** - HTTP client with rustls
- **Serde** - JSON serialization

### Frontend (React)
- **React 18** - UI framework
- **Vite** - Build tool
- **IBM Plex Mono** - Terminal font
- **Vanilla CSS** - No framework bloat

---

## 📁 Project Structure

```
betterbot/
├── rust-backend/           # Rust API server
│   ├── src/
│   │   ├── main.rs         # Entry point + scraping loops
│   │   ├── models.rs       # Signal types + config
│   │   ├── api/            # REST API routes
│   │   ├── scrapers/       # Hashdive + Polymarket clients
│   │   └── signals/        # Signal detection + storage
│   ├── Cargo.toml          # Rust dependencies
│   └── .env.example        # Environment variables template
│
├── terminal-ui/            # React frontend
│   ├── src/
│   │   ├── App.jsx         # Main component
│   │   ├── App.css         # Styling
│   │   └── index.css       # Global styles + CRT effects
│   ├── package.json        # Node dependencies
│   └── vite.config.js      # Build config
│
├── start.sh                # Launch script
└── README.md               # This file
```

---

## ⚙️ Configuration

### Environment Variables

Create `rust-backend/.env`:

```bash
# Required
HASHDIVE_API_KEY=your_api_key_here

# Optional (defaults shown)
DATABASE_PATH=./betterbot.db
PORT=8080
TWITTER_SCRAPE_INTERVAL=30
HASHDIVE_SCRAPE_INTERVAL=2700
HASHDIVE_WHALE_MIN_USD=10000
```

### Getting API Keys

1. **Hashdive API** - Sign up at https://hashdive.com
   - Free tier: 1000 calls/month
   - Premium: Unlimited (recommended)

---

## 🎨 UI Preview

### Header
```
[BETTERBOT]  ALPHA SIGNAL FEED // v2.0  [●ONLINE] [14:23:45]
```

### Signal Card
```
┌─────────────────────────────────────────────────────────────┐
│ 🐋 INSIDER EDGE                    Hashdive | 2m ago       │
├─────────────────────────────────────────────────────────────┤
│ BUY whale trade: $45,000 buy on 'Trump Election' outcome   │
│                                                             │
│ 📊 Trump wins 2024 election?                               │
├─────────────────────────────────────────────────────────────┤
│ CONFIDENCE  85%     ████████████████████░░                 │
└─────────────────────────────────────────────────────────────┘
```

### Footer
```
[SEE YOU SPACE COWBOY...]               PRESS CTRL+C TO EXIT
```

---

## 🔧 Development

### Build Backend
```bash
cd rust-backend
cargo build --release
cargo test
```

### Run Frontend Dev Server
```bash
cd terminal-ui
npm run dev
```

### Lint & Format
```bash
# Rust
cargo fmt
cargo clippy

# JavaScript
npm run lint
```

---

## 📡 API Endpoints

### GET /health
Health check

**Response:**
```json
{
  "status": "ok",
  "service": "betterbot-backend"
}
```

### GET /api/signals?limit=50
Fetch latest signals

**Response:**
```json
[
  {
    "id": 1,
    "signal_type": "InsiderEdge",
    "source": "Hashdive",
    "description": "BUY whale trade: $45,000...",
    "confidence": 85.0,
    "market_name": "Trump wins 2024?",
    "metadata": "{...}",
    "timestamp": "2025-10-29T12:34:56Z"
  }
]
```

### GET /api/stats
Get signal statistics

**Response:**
```json
{
  "total_signals": 156,
  "signals_24h": 23,
  "avg_confidence": 78.3,
  "by_type": [...],
  "by_source": [...]
}
```

---

## 🚢 Deployment

### Backend (VPS/Cloud)
```bash
# Build release binary
cd rust-backend
cargo build --release

# Binary location
./target/release/betterbot

# Run with environment variables
HASHDIVE_API_KEY=xxx ./target/release/betterbot
```

### Frontend (Vercel/Netlify)
```bash
cd terminal-ui
npm run build

# Deploy the ./dist folder
```

**Important:** Update `vite.config.js` proxy target to your backend URL

---

## 🔐 Security

- ✅ API keys in `.env` (not committed)
- ✅ `.gitignore` configured
- ✅ No hardcoded secrets
- ✅ HTTPS for production (recommended)
- ✅ Rate limiting on Hashdive API
- ✅ Input validation on all endpoints

---

## 🐛 Troubleshooting

### Backend won't start
```bash
# Check logs
tail -f /tmp/betterbot-backend.log

# Verify .env file exists
cat rust-backend/.env

# Test Hashdive API key
curl -H "x-api-key: YOUR_KEY" https://hashdive.com/api/get_api_usage
```

### Frontend won't connect
```bash
# Check backend is running
curl http://localhost:8080/health

# Check proxy configuration
cat terminal-ui/vite.config.js

# Clear node_modules
rm -rf terminal-ui/node_modules
cd terminal-ui && npm install
```

### No signals appearing
```bash
# Check database
sqlite3 rust-backend/betterbot.db "SELECT COUNT(*) FROM signals;"

# Verify scraping is running
curl http://localhost:8080/api/signals

# Check Hashdive credits
curl http://localhost:8080/api/stats
```

---

## 📚 Documentation

- [Signals Implementation](SIGNALS_2_3_COMPLETE.md)
- [UI Documentation](UI_COMPLETE.md)
- [Terminal UI README](terminal-ui/README.md)

---

## 🎯 Roadmap

### Day 4 (Next)
- [ ] Solana wallet integration
- [ ] $BETTER token verification (1000+ threshold)
- [ ] Token-gated API endpoints

### Day 5
- [ ] WebSocket real-time updates
- [ ] Browser notifications
- [ ] Signal filters

### Day 6
- [ ] Charts and analytics
- [ ] Export signals (CSV/JSON)
- [ ] Email alerts

### Day 7
- [ ] Production deployment
- [ ] Performance optimization
- [ ] Launch for $BETTER holders

---

## 📝 License

MIT License - See LICENSE file for details

---

## 🙏 Acknowledgments

- **Hashdive** - Whale trade data API
- **Polymarket** - Prediction market data
- **Cowboy Bebop** - Aesthetic inspiration

---

```
[SEE YOU SPACE COWBOY...]
```

**Built with 🤖 by BetterBot**
