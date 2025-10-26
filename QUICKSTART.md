# 🚀 BetterBot Quick Start

## Prerequisites
- Rust (1.70+)
- Hashdive API key ([get one here](https://hashdive.com))

## 5-Minute Setup

### 1. Clone & Configure
```bash
cd /Users/aryaman/betterbot/rust-backend
cp .env.example .env
nano .env  # Add your HASHDIVE_API_KEY
```

### 2. Run
```bash
cargo run
```

That's it! The bot is now running.

---

## Verify It's Working

### Check API Health
```bash
curl http://localhost:8080/health
```
**Expected:** `{"status":"healthy","version":"1.0"}`

### Get Signals
```bash
curl http://localhost:8080/api/signals
```
**Expected:** JSON array of signals (may be empty initially)

### Get Statistics
```bash
curl http://localhost:8080/api/stats
```
**Expected:** `{"total_signals": X, "by_type": {...}}`

---

## Watch the Logs

You should see:
```
🚀 BetterBot v2 starting up...
📋 Config loaded
💾 Database ready (0 signals stored)
🔑 Hashdive API connected: 45 credits used, ~955 remaining
📊 Polymarket client initialized
🌐 API server listening on http://0.0.0.0:8080
  - Health: http://localhost:8080/health
  - Signals: http://localhost:8080/api/signals
  - Stats: http://localhost:8080/api/stats
🔄 Starting scraping loops...
💡 Bot running on Hashdive whale trades + Polymarket signals
```

Every 2 minutes:
```
⏱️  Scrape cycle starting...
🐋 Fetched 23 whale trades
🐋 Whale signal #1: $12500 BUY
🎯 Whale cluster detected: 3 whales buying $45000 on Trump 2024
✅ Generated 2 signals this cycle
```

---

## Configuration Options

### Environment Variables (.env)

| Variable | Default | Description |
|----------|---------|-------------|
| `HASHDIVE_API_KEY` | *(required)* | Your Hashdive API key |
| `HASHDIVE_WHALE_MIN_USD` | `10000` | Minimum trade size to track |
| `HASHDIVE_SCRAPE_INTERVAL` | `120` | Seconds between scrapes |
| `DATABASE_PATH` | `./betterbot.db` | SQLite database location |
| `PORT` | `8080` | API server port |

### Example .env
```bash
HASHDIVE_API_KEY=9db4fbe868b312c7fe269d8e118d8b88cb74da8e8c2e3a48d1fafdeae3ca0f39
HASHDIVE_WHALE_MIN_USD=10000
HASHDIVE_SCRAPE_INTERVAL=120
DATABASE_PATH=./betterbot.db
PORT=8080
```

---

## Troubleshooting

### "No Hashdive API key configured"
**Solution:** Add `HASHDIVE_API_KEY` to your `.env` file

### "Hashdive API error: 401"
**Solution:** Check your API key is correct

### "Port 8080 already in use"
**Solution:** Change `PORT` in `.env` or kill the other process

### No signals appearing
**Normal!** It takes 2 minutes for first scrape cycle. Be patient.

---

## Production Deployment

### Build optimized binary
```bash
cargo build --release
```

Binary will be at: `target/release/betterbot`

### Run in production
```bash
./target/release/betterbot
```

### Run as background service (Linux/Mac)
```bash
nohup ./target/release/betterbot > betterbot.log 2>&1 &
```

---

## API Reference

### GET /health
Returns server health status

**Response:**
```json
{
  "status": "healthy",
  "version": "1.0"
}
```

### GET /api/signals
Get all signals (newest first)

**Query Parameters:**
- `limit` (optional): Max signals to return (default: 100)
- `offset` (optional): Pagination offset (default: 0)

**Response:**
```json
[
  {
    "id": 1,
    "signal_type": "insider_edge",
    "source": "Hashdive",
    "market_name": "Trump 2024",
    "description": "BUY whale trade: $15000 buy on 'Yes' outcome (55.23% price)",
    "confidence": 65.0,
    "metadata": "{...}",
    "created_at": "2025-10-23T15:30:00Z"
  }
]
```

### GET /api/signals/:id
Get specific signal by ID

### GET /api/stats
Get signal statistics

**Response:**
```json
{
  "total_signals": 42,
  "by_type": {
    "insider_edge": 35,
    "whale_cluster": 7
  }
}
```

---

## What's Working

✅ **Whale Following** - Tracks individual $10k+ trades  
✅ **Whale Cluster** - Detects 3+ whales consensus  
✅ **REST API** - Full signal access  
✅ **SQLite Storage** - Persistent signal database  

---

## What's Next

🔜 **Polymarket Events API** - Enable price deviation & expiry edge signals  
🔜 **Token Gating** - Require 1000+ $BETTER tokens  
🔜 **Frontend** - Next.js dashboard  

---

## Need Help?

- **Full docs:** See `README.md`
- **Cleanup report:** See `CLEANUP_COMPLETE.md`
- **Detailed status:** See `STATUS.md`
- **Code:** Browse `rust-backend/src/`

---

**Enjoy your Polymarket alpha signals! 🐋📈**
