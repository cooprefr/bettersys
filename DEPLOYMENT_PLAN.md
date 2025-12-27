# BETTERBOT DEPLOYMENT PLAN
**Production deployment to betterbot.win domain**

---

## Architecture Overview

```
betterbot.win (Landing Page)
    ↓
    |→ /app (Token-gated Terminal Interface)
    |→ Raydium DEX Link (Buy $BETTER)
```

---

## 1. Domain Structure

### Primary Domain: `betterbot.win`
- **Purpose**: Landing page & sales funnel
- **Content**:
  - Hero section with Cowboy Bebop aesthetic
  - Value proposition for alpha signal feed
  - Live stats ticker (total signals, win rate, etc.)
  - CTA: "Access Terminal" button
  - **Sales Funnel**: Directs to Raydium DEX to buy $BETTER token
  - Token requirements: Minimum 1000 $BETTER to access

### Subdomain: `app.betterbot.win`
- **Purpose**: Token-gated terminal interface
- **Content**:
  - Current purple terminal UI
  - Wallet connection modal (Phantom, Solflare, etc.)
  - Token balance verification
  - Real-time signal streaming (WebSocket or polling)

### API Subdomain: `api.betterbot.win`
- **Purpose**: Backend API server
- **Endpoints**:
  - `/health` - Health check
  - `/api/signals` - Get signals
  - `/api/stats` - Get statistics
  - `/auth/verify` - Verify wallet owns 1000+ $BETTER

---

## 2. Infrastructure Setup

### Option A: VPS Deployment (Recommended for MVP)
**Provider**: DigitalOcean, Linode, or AWS Lightsail

**Server Specs**:
- **CPU**: 2 vCPUs
- **RAM**: 4GB
- **Storage**: 80GB SSD
- **Cost**: ~$24/month

**Tech Stack**:
- **Reverse Proxy**: Nginx
- **SSL**: Let's Encrypt (free SSL certificates)
- **Process Manager**: systemd for backend, PM2 for frontend
- **Database**: SQLite (already using)

**Directory Structure**:
```
/opt/betterbot/
├── rust-backend/
│   ├── target/release/betterbot
│   ├── betterbot.db
│   └── .env
├── terminal-ui/
│   └── dist/
└── landing-page/
    └── dist/
```

### Option B: Serverless (For Scale)
- **Frontend**: Vercel (landing + app)
- **Backend**: AWS Lambda + API Gateway
- **Database**: AWS RDS or PlanetScale
- **Cost**: ~$0-50/month depending on usage

---

## 3. Landing Page Design

### betterbot.win Landing Page

**Design Elements** (Cowboy Bebop theme):
```
┌─────────────────────────────────────────────────┐
│                                                 │
│   [BETTERBOT]  ALPHA SIGNAL FEED v2.0          │
│                                                 │
│   "See you, space cowboy..."                   │
│                                                 │
│   ▓▓▓▓▓ POLYMARKET EDGE DETECTION ▓▓▓▓▓        │
│                                                 │
│   ✓ Whale Trade Tracking ($10k+ orders)        │
│   ✓ 45 Insider/Elite Wallets Monitored         │
│   ✓ Real-Time Arbitrage Detection              │
│   ✓ Quantitative Risk Scoring                  │
│                                                 │
│   LIVE STATS                                    │
│   ├─ 1,000+ signals generated                  │
│   ├─ 70.2% avg confidence                      │
│   └─ 45min update cycles                       │
│                                                 │
│   [ACCESS TERMINAL]  [BUY $BETTER TOKEN]       │
│                                                 │
│   Requirements: 1000 $BETTER tokens            │
│   Solana Contract: [CONTRACT_ADDRESS]          │
│   Buy on Raydium: [RAYDIUM_LINK]               │
│                                                 │
└─────────────────────────────────────────────────┘
```

**Key Sections**:
1. **Hero**: Bold headline, animated CRT scanlines
2. **Features**: 4-grid showcasing signal types
3. **Live Ticker**: Real-time stats from API
4. **Token Gate**: Clear path to acquiring $BETTER
5. **FAQ**: Explain signal types, risk levels, etc.
6. **Footer**: Social links, documentation

---

## 4. Token-Gated Access Flow

### Wallet Integration (Phantom Wallet)

**Libraries**:
```bash
npm install @solana/wallet-adapter-react @solana/wallet-adapter-wallets @solana/web3.js
```

**Flow**:
```
1. User visits app.betterbot.win
2. Modal: "Connect Wallet to Access Terminal"
3. User connects Phantom wallet
4. Frontend queries Solana RPC for token balance:
   - Token: $BETTER (SPL token)
   - Minimum: 1000 tokens
5a. If balance >= 1000: Show terminal interface
5b. If balance < 1000: Show "Insufficient Balance" + Buy link
```

**Token Verification Code** (React):
```javascript
import { Connection, PublicKey } from '@solana/web3.js';
import { TOKEN_PROGRAM_ID } from '@solana/spl-token';

const BETTER_TOKEN_MINT = new PublicKey('YOUR_TOKEN_MINT_ADDRESS');
const MIN_TOKENS = 1000;

async function verifyTokenBalance(walletAddress) {
  const connection = new Connection('https://api.mainnet-beta.solana.com');
  const walletPubkey = new PublicKey(walletAddress);
  
  // Get token accounts
  const tokenAccounts = await connection.getParsedTokenAccountsByOwner(
    walletPubkey,
    { programId: TOKEN_PROGRAM_ID }
  );
  
  // Find $BETTER token account
  for (const account of tokenAccounts.value) {
    const mintAddress = account.account.data.parsed.info.mint;
    if (mintAddress === BETTER_TOKEN_MINT.toString()) {
      const balance = account.account.data.parsed.info.tokenAmount.uiAmount;
      return balance >= MIN_TOKENS;
    }
  }
  
  return false; // No $BETTER tokens found
}
```

---

## 5. Deployment Steps

### Phase 1: Domain & DNS Setup
```bash
# 1. Purchase betterbot.win domain (Namecheap, GoDaddy, etc.)
# 2. Point DNS to your server IP:

A     @                     -> YOUR_SERVER_IP
A     app                   -> YOUR_SERVER_IP
A     api                   -> YOUR_SERVER_IP
CNAME www                   -> betterbot.win
```

### Phase 2: Server Setup
```bash
# SSH into server
ssh root@YOUR_SERVER_IP

# Install dependencies
apt update && apt upgrade -y
apt install -y nginx certbot python3-certbot-nginx nodejs npm build-essential sqlite3

# Install Rust (for backend)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# Create betterbot user
useradd -m -s /bin/bash betterbot
```

### Phase 3: Deploy Backend
```bash
# Clone or upload your code
cd /opt/betterbot
git clone YOUR_REPO_URL .

# Build backend
cd rust-backend
cargo build --release

# Create systemd service
sudo nano /etc/systemd/system/betterbot-backend.service
```

**betterbot-backend.service**:
```ini
[Unit]
Description=BetterBot Backend API
After=network.target

[Service]
Type=simple
User=betterbot
WorkingDirectory=/opt/betterbot/rust-backend
Environment=DATABASE_PATH=/opt/betterbot/rust-backend/betterbot.db
Environment=PORT=8080
ExecStart=/opt/betterbot/rust-backend/target/release/betterbot
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
```

```bash
# Enable and start
sudo systemctl daemon-reload
sudo systemctl enable betterbot-backend
sudo systemctl start betterbot-backend
```

### Phase 4: Deploy Frontend
```bash
# Build terminal UI
cd /opt/betterbot/terminal-ui
npm install
npm run build

# Build landing page (create separately)
cd /opt/betterbot/landing-page
npm install
npm run build
```

### Phase 5: Nginx Configuration
```bash
sudo nano /etc/nginx/sites-available/betterbot
```

**nginx config**:
```nginx
# API Subdomain
server {
    server_name api.betterbot.win;
    
    location / {
        proxy_pass http://localhost:8080;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection 'upgrade';
        proxy_set_header Host $host;
        proxy_cache_bypass $http_upgrade;
    }
}

# App Subdomain (Terminal)
server {
    server_name app.betterbot.win;
    root /opt/betterbot/terminal-ui/dist;
    index index.html;
    
    location / {
        try_files $uri $uri/ /index.html;
    }
}

# Main Domain (Landing Page)
server {
    server_name betterbot.win www.betterbot.win;
    root /opt/betterbot/landing-page/dist;
    index index.html;
    
    location / {
        try_files $uri $uri/ /index.html;
    }
}
```

```bash
# Enable site
sudo ln -s /etc/nginx/sites-available/betterbot /etc/nginx/sites-enabled/
sudo nginx -t
sudo systemctl reload nginx
```

### Phase 6: SSL Certificates
```bash
# Get SSL certs for all domains
sudo certbot --nginx -d betterbot.win -d www.betterbot.win
sudo certbot --nginx -d app.betterbot.win
sudo certbot --nginx -d api.betterbot.win

# Auto-renewal (already setup by certbot)
sudo certbot renew --dry-run
```

---

## 6. Token Launch Strategy

### Pre-Launch
1. **Create SPL Token on Solana**:
   ```bash
   spl-token create-token
   # Note the token mint address
   spl-token create-account YOUR_MINT_ADDRESS
   spl-token mint YOUR_MINT_ADDRESS 1000000000  # 1 billion supply
   ```

2. **Create Raydium Pool**:
   - Go to raydium.io
   - Create liquidity pool: $BETTER/SOL
   - Initial liquidity: e.g., 50M $BETTER + 10 SOL
   - Lock liquidity for credibility

3. **Marketing Materials**:
   - Token contract address
   - Raydium pool link
   - Twitter/X announcement
   - Discord community

### Launch Day
1. Announce token on Twitter/X
2. Post Raydium link on landing page
3. Enable wallet verification on app.betterbot.win
4. Monitor for bot activity/snipers

---

## 7. Frontend Updates for Token Gating

### app.betterbot.win (Add Wallet Connection)

**New Component**: `WalletGate.jsx`
```javascript
import { useWallet } from '@solana/wallet-adapter-react';
import { WalletMultiButton } from '@solana/wallet-adapter-react-ui';

function WalletGate({ children }) {
  const { connected, publicKey } = useWallet();
  const [hasAccess, setHasAccess] = useState(false);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (connected && publicKey) {
      checkAccess();
    }
  }, [connected, publicKey]);

  const checkAccess = async () => {
    setLoading(true);
    const hasTokens = await verifyTokenBalance(publicKey.toString());
    setHasAccess(hasTokens);
    setLoading(false);
  };

  if (!connected) {
    return <WalletConnectModal />;
  }

  if (loading) {
    return <LoadingScreen />;
  }

  if (!hasAccess) {
    return <InsufficientBalanceScreen />;
  }

  return children; // Show terminal
}
```

---

## 8. Cost Breakdown

### Development Costs
- Domain (betterbot.win): **$12/year**
- VPS (DigitalOcean): **$24/month**
- SSL: **Free** (Let's Encrypt)
- Total: **~$300/year** for infrastructure

### Token Launch Costs
- Solana SPL token creation: **~0.5 SOL** (~$75)
- Raydium liquidity pool: **10-50 SOL** ($1,500-$7,500)
- Marketing budget: **Variable** (Twitter ads, influencers, etc.)

---

## 9. Security Considerations

### Backend
- [ ] Rate limiting on API endpoints
- [ ] Input validation on all parameters
- [ ] HTTPS only (no HTTP)
- [ ] Environment variables for sensitive data
- [ ] Regular security updates

### Smart Contract
- [ ] Audit token contract before launch
- [ ] Verify on Solscan/Solana Explorer
- [ ] Lock liquidity for 6-12 months
- [ ] Revoke mint authority after initial supply

### Frontend
- [ ] CSP headers to prevent XSS
- [ ] Wallet signature verification
- [ ] No private keys stored client-side
- [ ] Rate limit wallet verification checks

---

## 10. Monitoring & Maintenance

### Application Monitoring
```bash
# Backend logs
sudo journalctl -u betterbot-backend -f

# Nginx logs
tail -f /var/log/nginx/access.log
tail -f /var/log/nginx/error.log

# System resources
htop
df -h
```

### Uptime Monitoring
- **UptimeRobot** (free): Monitor app.betterbot.win every 5 minutes
- **Alerts**: Email/SMS when site goes down

### Analytics
- **Plausible** or **Google Analytics**: Track landing page conversions
- **Custom metrics**: API calls, signal generation rate, wallet connections

---

## 11. Launch Checklist

### Pre-Launch
- [ ] Domain purchased and DNS configured
- [ ] Server provisioned and secured
- [ ] Backend deployed with systemd
- [ ] Frontend builds tested
- [ ] SSL certificates installed
- [ ] Token created on Solana
- [ ] Raydium pool created
- [ ] Landing page live with correct links
- [ ] Wallet integration tested on testnet

### Launch Day
- [ ] Announce on Twitter/X
- [ ] Post contract address
- [ ] Enable app.betterbot.win access
- [ ] Monitor server load
- [ ] Monitor Raydium pool activity
- [ ] Respond to community questions

### Post-Launch (Week 1)
- [ ] Fix any bugs reported
- [ ] Monitor signal generation
- [ ] Track token holder count
- [ ] Gather user feedback
- [ ] Plan v3 features

---

## 12. Next Steps (After MVP)

1. **Mobile App**: React Native version of terminal
2. **Discord Bot**: Post signals to Discord channel
3. **Telegram Bot**: Alerts for high-confidence signals
4. **API Access Tiers**:
   - 1,000 $BETTER: Basic access
   - 10,000 $BETTER: API access
   - 100,000 $BETTER: Premium features (custom alerts, etc.)
5. **DAO Governance**: Let token holders vote on tracked wallets

---

## Contact & Support

**Questions during deployment?**
- Reference this document
- Check logs for errors
- Test each phase before moving to next

**Good luck, space cowboy! 🚀**
