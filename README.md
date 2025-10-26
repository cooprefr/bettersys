# BetterBot - Polymarket Alpha Signal Bot

Token-gated alpha signal aggregator for $BETTER holders.

## 🎯 Current Status

**Production-ready MVP with 6 signal types:**
- ✅ **Whale Following** - $10k+ trades from Hashdive
- ✅ **Whale Cluster** - 3+ whales consensus detection
- ✅ **Price Deviation** - Binary arbitrage opportunities (implemented, needs Polymarket Events API)
- ✅ **Market Expiry Edge** - 95% confidence pre-close signals (implemented, needs Polymarket Events API)
- 🔜 **Volume Spike** - Planned for Day 3
- 🔜 **Spread Analysis** - Planned for Day 3

## Project Structure

```
betterbot/
├── rust-backend/          # Rust backend
│   └── src/
│       ├── scrapers/      # Hashdive + Polymarket API clients
│       ├── signals/       # 6 detection algorithms + SQLite storage
│       └── api/           # Axum REST API
├── frontend/              # (Planned) Next.js 14 dashboard
└── README.md
```

## Quick Start

### 1. Configure Environment
```bash
cd rust-backend
cp .env.example .env
```

Edit `.env`:
```bash
HASHDIVE_API_KEY=your_hashdive_key_here
HASHDIVE_WHALE_MIN_USD=10000
HASHDIVE_SCRAPE_INTERVAL=120
DATABASE_PATH=./betterbot.db
PORT=8080
```

### 2. Run the Bot
```bash
cargo run
```

### 3. Test the API
```bash
# Check health
curl http://localhost:8080/health

# Get all signals
curl http://localhost:8080/api/signals

# Get stats
curl http://localhost:8080/api/stats
```

## 📊 Signal Types

### 1. Whale Following (Active ✅)
- **Source:** Hashdive whale trades
- **Trigger:** $10k+ trades
- **Confidence:** 55-95% based on trade size

### 2. Whale Cluster (Active ✅)
- **Source:** Hashdive whale trades
- **Trigger:** 3+ whales trading same direction within 1 hour
- **Confidence:** 70% (3 whales) to 95% (6+ whales)

### 3. Price Deviation (Implemented ✅)
- **Source:** Polymarket markets
- **Trigger:** Yes + No prices deviate from $1.00 by 2%+
- **Confidence:** 30-95% based on deviation size

### 4. Market Expiry Edge (Implemented ✅)
- **Source:** Polymarket markets
- **Trigger:** Markets closing within 4 hours with 60%+ dominant side
- **Confidence:** 95% (based on historical accuracy)

## 🔧 API Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/health` | GET | Health check |
| `/api/signals` | GET | Get all signals (with pagination) |
| `/api/signals/:id` | GET | Get specific signal |
| `/api/stats` | GET | Signal statistics |

## 📈 Budget & Resources

- **Hashdive API:** 1000 credits/month (free tier)
- **Polymarket API:** Free, no auth required
- **Database:** SQLite (local storage)
- **Total monthly cost:** ~$0 🎉

## Development Roadmap

- ✅ **Day 1-2**: Core scraping + signal detection
- ✅ **Cleanup**: Removed Twitter, optimized codebase (-2,250 lines!)
- 🔜 **Day 3**: Polymarket Events API integration for signals 4 & 6
- 🔜 **Day 4**: Solana wallet + token-gating
- 🔜 **Day 5-6**: Frontend dashboard
- 🔜 **Day 7**: Deploy

## 🚀 Next Steps

See `CLEANUP_COMPLETE.md` for detailed cleanup report and integration instructions.
