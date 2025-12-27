# Deployment Guide: BetterBot on tradebetter.xyz

**Date:** November 25, 2025  
**Domain:** tradebetter.xyz  
**VPS Provider:** tradingvps.io  
**Stack:** Docker + Nginx + Let's Encrypt SSL

---

## Table of Contents

1. [Prerequisites](#1-prerequisites)
2. [VPS Setup (tradingvps.io)](#2-vps-setup-tradingvpsio)
3. [Domain DNS Configuration](#3-domain-dns-configuration)
4. [Docker Installation](#4-docker-installation)
5. [Project Structure for Deployment](#5-project-structure-for-deployment)
6. [Dockerfiles](#6-dockerfiles)
7. [Docker Compose Configuration](#7-docker-compose-configuration)
8. [Nginx Configuration](#8-nginx-configuration)
9. [SSL Certificate Setup](#9-ssl-certificate-setup)
10. [Environment Variables](#10-environment-variables)
11. [Deployment Commands](#11-deployment-commands)
12. [Verification & Testing](#12-verification--testing)
13. [Maintenance & Updates](#13-maintenance--updates)
14. [Troubleshooting](#14-troubleshooting)

---

## 1. Prerequisites

### Local Machine Requirements:
- Git installed
- SSH client (Terminal on Mac/Linux, PuTTY on Windows)
- Your BetterBot repository ready

### Accounts Needed:
- [x] tradingvps.io account with VPS purchased
- [x] Domain tradebetter.xyz registered
- [ ] Cloudflare account (recommended for DNS management)
- [x] API keys: HASHDIVE_API_KEY, DOME_API_KEY

### VPS Minimum Specs:
- **OS:** Ubuntu 22.04 LTS or 24.04 LTS
- **RAM:** 2GB minimum (4GB recommended)
- **CPU:** 2 vCPU
- **Storage:** 20GB SSD
- **Ports:** 22 (SSH), 80 (HTTP), 443 (HTTPS)

---

## 2. VPS Setup (tradingvps.io)

### Step 2.1: Order VPS
1. Go to https://tradingvps.io
2. Select a plan with minimum 2GB RAM
3. Choose **Ubuntu 22.04 LTS** as the OS
4. Complete purchase and wait for setup email

### Step 2.2: Access Your VPS
You'll receive an email with:
- IP Address (e.g., `123.45.67.89`)
- Username (usually `root`)
- Password

### Step 2.3: SSH into Your VPS
```bash
# From your local machine
ssh root@YOUR_VPS_IP

# Example:
ssh root@123.45.67.89
```

### Step 2.4: Initial Server Setup
```bash
# Update system packages
apt update && apt upgrade -y

# Install essential tools
apt install -y curl wget git nano ufw

# Set timezone
timedatectl set-timezone UTC

# Create a non-root user (recommended)
adduser betterbot
usermod -aG sudo betterbot

# Enable firewall
ufw allow OpenSSH
ufw allow 80/tcp
ufw allow 443/tcp
ufw enable

# Verify firewall status
ufw status
```

### Step 2.5: Set Up SSH Key Authentication (Recommended)
```bash
# On your LOCAL machine, generate SSH key if you don't have one
ssh-keygen -t ed25519 -C "your_email@example.com"

# Copy your public key to the VPS
ssh-copy-id root@YOUR_VPS_IP

# Test key-based login
ssh root@YOUR_VPS_IP
```

---

## 3. Domain DNS Configuration

### Option A: Using Cloudflare (Recommended)

#### Step 3.1: Add Domain to Cloudflare
1. Log in to https://dash.cloudflare.com
2. Click "Add a Site" → Enter `tradebetter.xyz`
3. Select Free plan (sufficient for this use case)
4. Cloudflare will scan existing DNS records

#### Step 3.2: Update Nameservers
1. Go to your domain registrar (where you bought tradebetter.xyz)
2. Update nameservers to Cloudflare's provided ones:
   - `ns1.cloudflare.com` (example - use the ones Cloudflare gives you)
   - `ns2.cloudflare.com`
3. Wait 24-48 hours for propagation (usually faster)

#### Step 3.3: Add DNS Records in Cloudflare
```
Type    Name    Content           Proxy Status
A       @       YOUR_VPS_IP       Proxied (orange cloud)
A       www     YOUR_VPS_IP       Proxied (orange cloud)
A       api     YOUR_VPS_IP       Proxied (orange cloud)
```

### Option B: Direct DNS at Registrar
If not using Cloudflare, add these records at your registrar:
```
Type    Name    Value           TTL
A       @       YOUR_VPS_IP     3600
A       www     YOUR_VPS_IP     3600
A       api     YOUR_VPS_IP     3600
```

---

## 4. Docker Installation

SSH into your VPS and run:

```bash
# Install Docker
curl -fsSL https://get.docker.com -o get-docker.sh
sudo sh get-docker.sh

# Add your user to docker group (if using non-root user)
sudo usermod -aG docker $USER

# Install Docker Compose
sudo curl -L "https://github.com/docker/compose/releases/latest/download/docker-compose-$(uname -s)-$(uname -m)" -o /usr/local/bin/docker-compose
sudo chmod +x /usr/local/bin/docker-compose

# Verify installations
docker --version
docker-compose --version

# Start Docker service
sudo systemctl enable docker
sudo systemctl start docker
```

---

## 5. Project Structure for Deployment

Create this structure on your VPS:

```bash
mkdir -p /opt/betterbot
cd /opt/betterbot
```

The deployment structure:
```
/opt/betterbot/
├── docker-compose.yml          # Main orchestration file
├── .env                        # Environment variables (secrets)
├── nginx/
│   ├── nginx.conf              # Main nginx config
│   └── conf.d/
│       └── default.conf        # Site configuration
├── certbot/
│   ├── conf/                   # SSL certificates (auto-generated)
│   └── www/                    # ACME challenge files
├── rust-backend/
│   ├── Dockerfile
│   ├── src/
│   ├── Cargo.toml
│   └── .env                    # Backend-specific env
├── frontend/
│   ├── Dockerfile
│   ├── src/
│   ├── package.json
│   └── nginx.conf              # Frontend nginx config
└── data/
    ├── signals.db              # SQLite database (persistent)
    └── auth.db                 # Auth database (persistent)
```

### Clone Your Repository
```bash
cd /opt/betterbot
git clone https://github.com/YOUR_USERNAME/betterbot.git .
# Or copy files via scp:
# scp -r /Users/aryaman/betterbot/* root@YOUR_VPS_IP:/opt/betterbot/
```

---

## 6. Dockerfiles

### 6.1: Backend Dockerfile (`rust-backend/Dockerfile`)

```dockerfile
# Stage 1: Build
FROM rust:1.75-bookworm as builder

WORKDIR /usr/src/app

# Install dependencies for SQLite
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    libsqlite3-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy manifests first for better caching
COPY Cargo.toml Cargo.lock ./

# Create dummy src to cache dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release
RUN rm -rf src

# Copy actual source
COPY src ./src

# Build the actual application
RUN touch src/main.rs && cargo build --release

# Stage 2: Runtime
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    libsqlite3-0 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy binary from builder
COPY --from=builder /usr/src/app/target/release/betterbot-backend /app/betterbot

# Create data directory
RUN mkdir -p /app/data

# Expose port
EXPOSE 3000

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:3000/health || exit 1

# Run the application
CMD ["./betterbot"]
```

### 6.2: Frontend Dockerfile (`frontend/Dockerfile`)

```dockerfile
# Stage 1: Build
FROM node:20-alpine as builder

WORKDIR /app

# Copy package files
COPY package*.json ./

# Install dependencies
RUN npm ci

# Copy source
COPY . .

# Build arguments for environment
ARG VITE_API_URL
ARG VITE_WS_URL

ENV VITE_API_URL=$VITE_API_URL
ENV VITE_WS_URL=$VITE_WS_URL

# Build the app
RUN npm run build

# Stage 2: Serve with nginx
FROM nginx:alpine

# Copy built files
COPY --from=builder /app/dist /usr/share/nginx/html

# Copy nginx config
COPY nginx.conf /etc/nginx/conf.d/default.conf

# Expose port
EXPOSE 80

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD wget --quiet --tries=1 --spider http://localhost:80/ || exit 1

CMD ["nginx", "-g", "daemon off;"]
```

### 6.3: Frontend Nginx Config (`frontend/nginx.conf`)

```nginx
server {
    listen 80;
    server_name localhost;
    root /usr/share/nginx/html;
    index index.html;

    # Gzip compression
    gzip on;
    gzip_types text/plain text/css application/json application/javascript text/xml application/xml;

    # Handle SPA routing
    location / {
        try_files $uri $uri/ /index.html;
    }

    # Cache static assets
    location ~* \.(js|css|png|jpg|jpeg|gif|ico|svg|woff|woff2)$ {
        expires 1y;
        add_header Cache-Control "public, immutable";
    }
}
```

---

## 7. Docker Compose Configuration

Create `/opt/betterbot/docker-compose.yml`:

```yaml
version: '3.8'

services:
  # Rust Backend
  backend:
    build:
      context: ./rust-backend
      dockerfile: Dockerfile
    container_name: betterbot-backend
    restart: unless-stopped
    environment:
      - DATABASE_PATH=/app/data/betterbot_signals.db
      - AUTH_DB_PATH=/app/data/betterbot_auth.db
      - HASHDIVE_API_KEY=${HASHDIVE_API_KEY}
      - DOME_API_KEY=${DOME_API_KEY}
      - JWT_SECRET=${JWT_SECRET}
      - RUST_LOG=info,betterbot=debug
      - PORT=3000
    volumes:
      - ./data:/app/data
    networks:
      - betterbot-network
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:3000/health"]
      interval: 30s
      timeout: 10s
      retries: 3

  # React Frontend
  frontend:
    build:
      context: ./frontend
      dockerfile: Dockerfile
      args:
        - VITE_API_URL=https://tradebetter.xyz/api
        - VITE_WS_URL=wss://tradebetter.xyz/ws
    container_name: betterbot-frontend
    restart: unless-stopped
    networks:
      - betterbot-network
    depends_on:
      - backend

  # Nginx Reverse Proxy
  nginx:
    image: nginx:alpine
    container_name: betterbot-nginx
    restart: unless-stopped
    ports:
      - "80:80"
      - "443:443"
    volumes:
      - ./nginx/conf.d:/etc/nginx/conf.d:ro
      - ./certbot/conf:/etc/letsencrypt:ro
      - ./certbot/www:/var/www/certbot:ro
    networks:
      - betterbot-network
    depends_on:
      - backend
      - frontend
    command: '/bin/sh -c ''while :; do sleep 6h & wait $${!}; nginx -s reload; done & nginx -g "daemon off;"'''

  # Certbot for SSL
  certbot:
    image: certbot/certbot
    container_name: betterbot-certbot
    restart: unless-stopped
    volumes:
      - ./certbot/conf:/etc/letsencrypt
      - ./certbot/www:/var/www/certbot
    entrypoint: "/bin/sh -c 'trap exit TERM; while :; do certbot renew; sleep 12h & wait $${!}; done;'"

networks:
  betterbot-network:
    driver: bridge
```

---

## 8. Nginx Configuration

### 8.1: Initial Config (HTTP only - for SSL setup)

Create `/opt/betterbot/nginx/conf.d/default.conf`:

```nginx
# HTTP server - for initial SSL certificate setup
server {
    listen 80;
    server_name tradebetter.xyz www.tradebetter.xyz;

    # ACME challenge for Let's Encrypt
    location /.well-known/acme-challenge/ {
        root /var/www/certbot;
    }

    # Redirect all other traffic to HTTPS (after SSL is set up)
    location / {
        return 301 https://$host$request_uri;
    }
}
```

### 8.2: Full Config (After SSL setup)

Replace the above with this after getting SSL certificates:

```nginx
# HTTP - Redirect to HTTPS
server {
    listen 80;
    server_name tradebetter.xyz www.tradebetter.xyz;

    location /.well-known/acme-challenge/ {
        root /var/www/certbot;
    }

    location / {
        return 301 https://$host$request_uri;
    }
}

# HTTPS - Main server
server {
    listen 443 ssl http2;
    server_name tradebetter.xyz www.tradebetter.xyz;

    # SSL certificates
    ssl_certificate /etc/letsencrypt/live/tradebetter.xyz/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/tradebetter.xyz/privkey.pem;

    # SSL settings
    ssl_protocols TLSv1.2 TLSv1.3;
    ssl_prefer_server_ciphers off;
    ssl_ciphers ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384;

    # Security headers
    add_header X-Frame-Options "SAMEORIGIN" always;
    add_header X-Content-Type-Options "nosniff" always;
    add_header X-XSS-Protection "1; mode=block" always;
    add_header Referrer-Policy "strict-origin-when-cross-origin" always;

    # Frontend (React app)
    location / {
        proxy_pass http://frontend:80;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }

    # Backend API
    location /api/ {
        proxy_pass http://backend:3000/api/;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }

    # WebSocket endpoint
    location /ws {
        proxy_pass http://backend:3000/ws;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_read_timeout 86400;
    }

    # Health check endpoint
    location /health {
        proxy_pass http://backend:3000/health;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
    }
}
```

---

## 9. SSL Certificate Setup

### Step 9.1: Create Required Directories
```bash
cd /opt/betterbot
mkdir -p certbot/conf certbot/www nginx/conf.d
```

### Step 9.2: Start Nginx with HTTP-only config
```bash
# Make sure the initial HTTP-only config is in place
docker-compose up -d nginx
```

### Step 9.3: Obtain SSL Certificate
```bash
# Run certbot to get the initial certificate
docker-compose run --rm certbot certonly \
    --webroot \
    --webroot-path=/var/www/certbot \
    --email your-email@example.com \
    --agree-tos \
    --no-eff-email \
    -d tradebetter.xyz \
    -d www.tradebetter.xyz
```

### Step 9.4: Update Nginx Config
After successful certificate issuance, update `/opt/betterbot/nginx/conf.d/default.conf` with the full HTTPS configuration from section 8.2.

### Step 9.5: Restart Services
```bash
docker-compose down
docker-compose up -d
```

---

## 10. Environment Variables

Create `/opt/betterbot/.env`:

```bash
# API Keys (REQUIRED - get from your accounts)
HASHDIVE_API_KEY=your_hashdive_api_key_here
DOME_API_KEY=your_dome_api_key_here

# JWT Secret (generate a secure random string)
# Run: openssl rand -base64 32
JWT_SECRET=your_secure_jwt_secret_minimum_32_characters

# Database paths (inside container)
DATABASE_PATH=/app/data/betterbot_signals.db
AUTH_DB_PATH=/app/data/betterbot_auth.db

# Risk Management
INITIAL_BANKROLL=10000
KELLY_FRACTION=0.25

# Logging
RUST_LOG=info,betterbot=debug
```

**IMPORTANT:** Never commit `.env` to git! Add it to `.gitignore`.

---

## 11. Deployment Commands

### First Time Deployment
```bash
# SSH into your VPS
ssh root@YOUR_VPS_IP

# Navigate to project
cd /opt/betterbot

# Create .env file with your secrets
nano .env

# Build all images
docker-compose build

# Start services (detached)
docker-compose up -d

# Check logs
docker-compose logs -f

# Check status
docker-compose ps
```

### Updating the Application
```bash
cd /opt/betterbot

# Pull latest code
git pull origin main

# Rebuild and restart
docker-compose build
docker-compose up -d

# View logs
docker-compose logs -f
```

### Useful Docker Commands
```bash
# View all containers
docker ps -a

# View logs for specific service
docker-compose logs -f backend
docker-compose logs -f frontend
docker-compose logs -f nginx

# Restart a specific service
docker-compose restart backend

# Stop all services
docker-compose down

# Stop and remove volumes (CAUTION: deletes data)
docker-compose down -v

# Enter container shell
docker exec -it betterbot-backend /bin/bash
docker exec -it betterbot-frontend /bin/sh

# Check database
docker exec -it betterbot-backend sqlite3 /app/data/betterbot_signals.db "SELECT COUNT(*) FROM signals;"
```

---

## 12. Verification & Testing

### Step 12.1: Check Services
```bash
# All containers should be "Up"
docker-compose ps

# Expected output:
# NAME                  STATUS
# betterbot-backend     Up (healthy)
# betterbot-frontend    Up (healthy)
# betterbot-nginx       Up
# betterbot-certbot     Up
```

### Step 12.2: Test Endpoints
```bash
# Health check
curl https://tradebetter.xyz/health

# API test (should return "Missing authorization token" without auth)
curl https://tradebetter.xyz/api/signals

# SSL certificate check
curl -vI https://tradebetter.xyz 2>&1 | grep -i "SSL certificate"
```

### Step 12.3: Test in Browser
1. Open https://tradebetter.xyz
2. You should see the login screen
3. Login with: `admin` / `admin123`
4. Signals should appear and update in real-time
5. WebSocket should show "Connected" status

### Step 12.4: Test WebSocket
```bash
# Install wscat if needed
npm install -g wscat

# Test WebSocket connection (will fail without auth, but tests connectivity)
wscat -c wss://tradebetter.xyz/ws
```

---

## 13. Maintenance & Updates

### Automatic SSL Renewal
SSL certificates auto-renew via the certbot container (checks every 12 hours).

### Database Backups
```bash
# Create backup
docker exec betterbot-backend sqlite3 /app/data/betterbot_signals.db ".backup '/app/data/backup_$(date +%Y%m%d).db'"

# Copy backup to local machine
scp root@YOUR_VPS_IP:/opt/betterbot/data/backup_*.db ./backups/
```

### Log Rotation
Add to `/etc/logrotate.d/docker-containers`:
```
/var/lib/docker/containers/*/*.log {
    rotate 7
    daily
    compress
    missingok
    delaycompress
    copytruncate
}
```

### Monitoring
Consider setting up:
- **Uptime monitoring:** UptimeRobot (free) or Better Stack
- **Log aggregation:** Papertrail or Logtail
- **Metrics:** Prometheus + Grafana (advanced)

---

## 14. Troubleshooting

### Issue: Container won't start
```bash
# Check logs
docker-compose logs backend
docker-compose logs frontend

# Common fixes:
# - Check .env file exists and has correct values
# - Ensure ports aren't in use: `netstat -tlnp | grep -E '80|443|3000'`
# - Check disk space: `df -h`
```

### Issue: SSL certificate errors
```bash
# Check certificate status
docker-compose run --rm certbot certificates

# Force renewal
docker-compose run --rm certbot renew --force-renewal

# Check nginx config
docker exec betterbot-nginx nginx -t
```

### Issue: WebSocket not connecting
```bash
# Check backend is running
curl http://localhost:3000/health

# Check nginx WebSocket config
docker exec betterbot-nginx cat /etc/nginx/conf.d/default.conf | grep -A 10 "location /ws"

# Check for firewall issues
ufw status
```

### Issue: Database errors
```bash
# Check database file permissions
ls -la /opt/betterbot/data/

# Verify database integrity
docker exec betterbot-backend sqlite3 /app/data/betterbot_signals.db "PRAGMA integrity_check;"
```

### Issue: High memory usage
```bash
# Check container stats
docker stats

# Limit container memory in docker-compose.yml:
# services:
#   backend:
#     deploy:
#       resources:
#         limits:
#           memory: 512M
```

---

## Quick Reference Commands

```bash
# Start everything
cd /opt/betterbot && docker-compose up -d

# Stop everything
cd /opt/betterbot && docker-compose down

# View all logs
cd /opt/betterbot && docker-compose logs -f

# Rebuild after code changes
cd /opt/betterbot && docker-compose build && docker-compose up -d

# Check SSL cert expiry
docker-compose run --rm certbot certificates

# Database signal count
docker exec betterbot-backend sqlite3 /app/data/betterbot_signals.db "SELECT COUNT(*) FROM signals;"

# Restart backend only
docker-compose restart backend
```

---

## Summary Checklist

- [ ] VPS ordered from tradingvps.io
- [ ] SSH access configured
- [ ] Firewall configured (ports 22, 80, 443)
- [ ] Docker & Docker Compose installed
- [ ] Domain DNS pointing to VPS IP
- [ ] SSL certificates obtained via Let's Encrypt
- [ ] `.env` file created with API keys
- [ ] All containers running (`docker-compose ps`)
- [ ] Website accessible at https://tradebetter.xyz
- [ ] Login working with admin/admin123
- [ ] Signals appearing in real-time
- [ ] WebSocket showing "Connected"

---

**Deployment Complete!** Your BetterBot instance should now be live at https://tradebetter.xyz
