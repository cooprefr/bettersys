# AWS Server Connection

**IP:** 54.194.99.177  
**User:** ec2-user  
**Key:** ~/Downloads/key.pem

## SSH Command
```bash
ssh -i ~/Downloads/key.pem ec2-user@54.194.99.177
```

## After Connecting - Pull & Build
```bash
cd ~/bettersys && git pull origin main && cd rust-backend && cargo build --release
```

## Run Backend
```bash
cd ~/bettersys/rust-backend && cargo run --release
```

## Run Specific Binaries
```bash
# Binance ingest benchmark
cargo run --release --bin binance_ingest_bench

# Route quality monitor
cargo run --release --bin route_quality_monitor

# Edge receiver
cargo run --release --bin edge_receiver
```

## Apply System Tuning (requires sudo)
```bash
cd ~/bettersys/scripts/tuning && sudo ./deploy_tuning.sh
```
