# Binance Price Feed Architecture - LaTeX Documentation

This folder contains comprehensive LaTeX documentation for the Binance price feed architecture in BetterBot.

## Files

- `main.tex` - Complete LaTeX document with:
  - System architecture overview
  - Layer-by-layer component descriptions
  - Data flow diagrams (TikZ)
  - SeqLock synchronization details
  - Latency measurement harness specification
  - Expected latency ranges
  - Configuration reference

## Building the PDF

### Prerequisites

Install a LaTeX distribution:
- **macOS**: `brew install mactex` or MacTeX from https://tug.org/mactex/
- **Ubuntu/Debian**: `sudo apt install texlive-full`
- **Fedora**: `sudo dnf install texlive-scheme-full`

### Build Command

```bash
cd docs/binance_architecture_latex
pdflatex main.tex
pdflatex main.tex  # Run twice for TOC
```

Or use `latexmk` for automatic rebuilds:
```bash
latexmk -pdf main.tex
```

### Required Packages

The document uses these LaTeX packages (included in full distributions):
- `tikz` with libraries: shapes, arrows, positioning, fit, backgrounds, calc, decorations
- `listings` - Code listings
- `tcolorbox` - Colored boxes
- `hyperref` - Hyperlinks
- `booktabs` - Professional tables
- `fancyhdr` - Headers/footers

## Document Structure

1. **Executive Summary** - Key performance metrics and layer overview
2. **System Architecture** - High-level data flow diagram
3. **Layer 1: BinancePriceFeed** - barter-data based production feed
4. **Layer 2: BinanceBookTickerFeed** - SIMD-optimized feed
5. **Layer 3: BinanceHftIngest** - Zero-allocation SeqLock architecture
6. **Layer 4: HardenedBinanceIngest** - Production wrapper with state machine
7. **Latency Measurement Harness** - Instrumentation and CSV export
8. **Expected Latency Ranges** - AWS EU-West-1 benchmarks
9. **Edge Receiver Architecture** - UDP multicast for ultra-low latency
10. **Configuration Reference** - Environment variables

## Related Source Files

- `rust-backend/src/scrapers/binance_price_feed.rs`
- `rust-backend/src/scrapers/binance_book_ticker.rs`
- `rust-backend/src/scrapers/binance_hft_ingest.rs`
- `rust-backend/src/scrapers/binance_hardened_ingest.rs`
- `rust-backend/src/scrapers/binance_session.rs`
- `rust-backend/src/performance/latency/binance_harness.rs`
- `rust-backend/src/edge/client.rs`
