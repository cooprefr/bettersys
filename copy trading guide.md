
## How Copy Trading Works

Choose a trader to copy - someone with proven skill, discipline, or an edge in a domain.

Allocate a bankroll - funds dedicated only to copying that trader.

Set % per trade - how much of your bankroll you are comfortable risking per trade.

Set copy % - how closely you mirror the trader's size.

Choose exit mode:
- Mirror: You exit when they exit
- Proportional: You scale out based on the percentage of their reduce/close.
- Manual: You manage exits yourself.

Add advanced filters to avoid bad fills: liquidity, slippage, time-to-resolution, volume, max per market, price ranges.

## Advanced Settings

Max amount per market ($) – Max total exposure you are willing to have in a single market.

Min amount per market ($) – Minimum total exposure you want in a market before it is "worth it" for you; below this, trades are skipped.

Max copy amount per trade ($) – Hard dollar cap per copied trade.

Min volume of each market ($) – Required traded volume before you will copy into that market.

Min liquidity per market ($) – Required book liquidity; avoids thin books.

Market price range (Min / Max, in ¢) – Only copy trades in this price band.

Max slippage per market (¢) – Maximum price movement you will tolerate from expected to actual fill price.

Max time until resolution (days) – Only copy trades in markets resolving within this many days.

Used well, copy trading becomes a risk-controlled way to participate in markets strategically while learning from experienced traders.

## Trader Profiles and Recommended Settings

(Assume examples for a $1,000 copy bankroll; scale linearly up or down.)

### 1. High-Frequency Trader (HFT / Micro Scalper)

Trades BTC up/down and very short-dated markets dozens of times per day.

Risks: Slippage and latency kill edge. Many tiny trades; without caps you churn capital.

Result: You participate in their best moves but your per-trade and per-market risk stays tightly bounded.

| Setting                        | Recommendation      |
| ---                            | ---                 |
| Bankroll                       | $250–$500 (start small) |
| % Size for each trade          | 1–2%                |
| Max % per trade                | 20–30%              |
| Exit mode                      | Proportional        |
| Max amount per market          | $40–$60             |
| Max copy amount per trade      | $5–$15              |
| Min volume of each market      | $3,000+             |
| Min liquidity per market       | $1,000–$2,000       |
| Market price range             | Min 40¢, Max 65¢    |
| Max slippage per market        | 3–5¢                |
| Max time until resolution      | 1 day               |

### 2. Sports Bettor (Pre-Match + Live)

Lower frequency, high-conviction bets on games and props.

Risks: Single games can be large relative to bankroll. Live lines move fast around injuries and momentum.

Result: Strong upside when copying a sharp sports bettor without putting half your bankroll on a single match.

| Setting                        | Recommendation                  |
| ---                            | ---                             |
| Bankroll                       | $500–$1,500                     |
| % Size for each trade          | 5–10%                           |
| Max % per trade                | 50–100%                         |
| Exit mode                      | Manual or Mirror                |
| Max amount per market          | $200–$300                       |
| Max copy amount per trade      | $75–$250 (depending on bankroll)|
| Min volume of each market      | $10,000+                        |
| Max slippage per market        | 2–6¢                            |
| Max time until resolution      | 7–30 days                       |

### 3. Political Long-Horizon Bettor

Builds positions early in elections or policy markets and holds for months.

Risks: Capital locked for long periods. Polls and narratives can shift sharply.

Result: You ride their long-term thesis while avoiding over-concentration in a single race.

| Setting                        | Recommendation      |
| ---                            | ---                 |
| Bankroll                       | $1,000+ (only what you're happy to lock) |
| % Size for each trade          | 3–5%                |
| Max % per trade                | 40–60%              |
| Exit mode                      | Mirror or Proportional |
| Max amount per market          | $200–$300           |
| Min volume of each market      | $20,000+            |
| Market price range             | Min 25¢, Max 75¢    |
| Max slippage per market        | 2–4¢                |
| Max time until resolution      | 60–180 days         |

### 4. Liquidity Provider / Market Maker

Quotes both sides, buys dips and sells rips to earn spread.

Risks: Many small fills on both sides. Thin books can cause nasty fills.

Result: You benefit from their range-trading edge without becoming unintended deep liquidity.

| Setting                        | Recommendation      |
| ---                            | ---                 |
| Bankroll                       | $500–$1,000         |
| % Size for each trade          | 1–2%                |
| Max % per trade                | 10–30%              |
| Exit mode                      | Proportional        |
| Max copy amount per trade      | $10–$25             |
| Max amount per market          | $75–$125            |
| Min liquidity per market       | $2,000–$5,000       |
| Max slippage per market        | 1–2¢                |
| Max time until resolution      | 30–90 days          |

### 5. Whale Swing Trader

Places a few large, directional bets with strong conviction.

Risks: Copying too much size on a single trade blows up bankroll.

Result: You ride their big directional trades with bounded dollar risk.

| Setting                        | Recommendation      |
| ---                            | ---                 |
| Bankroll                       | $1,000+             |
| % Size for each trade          | 3–6%                |
| Max % per trade                | 15–35%              |
| Exit mode                      | Mirror              |
| Max copy amount per trade      | $50–$150            |
| Max amount per market          | $200–$300           |
| Min volume of each market      | $20,000+            |
| Max slippage per market        | 2–4¢                |
| Max time until resolution      | 30–90 days          |

### 6. Insider / Early-Info Trader

Enters before public news (court filings, injury leaks, etc.).

Risks: Edge evaporates if you get filled late. Slippage is everything.

Result: You benefit when you're early, and slippage rules protect you when you're late.

| Setting                        | Recommendation      |
| ---                            | ---                 |
| Bankroll                       | $300–$800           |
| % Size for each trade          | 2–4%                |
| Max % per trade                | 25–40%              |
| Exit mode                      | Proportional or Manual |
| Max copy amount per trade      | $20–$40             |
| Min volume of each market      | $10,000+            |
| Min liquidity per market       | $3,000+             |
| Market price range             | Min 20¢, Max 70¢    |
| Max slippage per market        | 1–3¢                |
| Max time until resolution      | 7–30 days           |

### 7. Quant / Data-Model Trader

Stat-driven, diversified across many markets.

Risks: Edges are small but steady; needs time.

Result: Good long-term "index-like" compounding profile.

| Setting                        | Recommendation      |
| ---                            | ---                 |
| Bankroll                       | $1,000+             |
| % Size for each trade          | 3–6%                |
| Max % per trade                | 50–100%             |
| Exit mode                      | Mirror or Proportional |
| Max amount per market          | $150–$200           |
| Min volume of each market      | $5,000–$10,000      |
| Max slippage per market        | 2–4¢                |
| Max time until resolution      | 30–120 days         |

### 8. Meme / Hype Trader

Targets celebrity trials, viral scandals, AI drama.

Risks: Huge volatility and narrative whiplash.

Result: You get upside in the wild stuff without letting one meme nuke your bankroll.

| Setting                        | Recommendation      |
| ---                            | ---                 |
| Bankroll                       | $200–$500           |
| % Size for each trade          | 1–3%                |
| Max % per trade                | 20–40%              |
| Exit mode                      | Manual              |
| Max amount per market          | $50–$100            |
| Market price range             | Min 10¢, Max 85¢ (avoid > 85¢) |
| Max slippage per market        | 2–4¢                |
| Max time until resolution      | 7–30 days           |

### 9. Short-Term Event Trader (CPI / FOMC / Election Night)

Only trades very near catalysts.

Risks: High volatility in short window. Need tight time filters.

Result: Focused exposure to peak volatility windows only.

| Setting                        | Recommendation      |
| ---                            | ---                 |
| Bankroll                       | $500–$1,000         |
| % Size for each trade          | 4–8%                |
| Max % per trade                | 40–70%              |
| Exit mode                      | Mirror              |
| Max amount per market          | $200–$250           |
| Min volume of each market      | $20,000+            |
| Max slippage per market        | 2–5¢                |
| Max time until resolution      | 1–7 days            |

### 10. Long-Only Accumulator (DCA Style)

Gradually buys dips in one outcome.

Risks: High concentration over time.

Result: Controlled averaging into a long-term thesis.

| Setting                        | Recommendation      |
| ---                            | ---                 |
| Bankroll                       | $1,000+             |
| % Size for each trade          | 2–4%                |
| Max % per trade                | 30–60%              |
| Exit mode                      | Manual              |
| Max amount per market          | $200–$300           |
| Min amount per market          | $25–$50             |
| Max time until resolution      | 60–180 days         |

### 11. Parlay / Multi-Leg Outcome Trader

Builds correlated baskets (team wins + player scores + over, etc.).

Risks: Low hit-rate, high payoff when it hits.

Result: Lottery-style upside with hard caps on damage.

| Setting                        | Recommendation      |
| ---                            | ---                 |
| Bankroll                       | $250–$750           |
| % Size for each trade          | 1–3%                |
| Max % per trade                | 15–35%              |
| Exit mode                      | Manual or Mirror    |
| Max copy amount per trade      | $10–$30             |
| Max amount per market          | $75–$125            |
| Min amount per market          | $10–$20             |
| Max time until resolution      | 7–30 days           |

### 12. Range / Mean-Reversion Trader

Buys when price crashes, sells on rebounds.

Risks: Trending markets blow through ranges.

Result: Systematic harvesting of choppy markets.

| Setting                        | Recommendation      |
| ---                            | ---                 |
| Bankroll                       | $500–$1,000         |
| % Size for each trade          | 2–5%                |
| Max % per trade                | 30–60%              |
| Exit mode                      | Mirror              |
| Max amount per market          | $150–$200           |
| Min liquidity per market       | $3,000+             |
| Market price range             | Min 25¢, Max 75¢    |
| Max slippage per market        | 1–3¢                |
| Max time until resolution      | 30–90 days          |

### 13. Early Poll Analyst (Polling-Driven Politics)

Builds positions based on polling models long before peak attention.

Risks: Long durations, evolving narratives.

Result: Deep macro/political exposure with sensible caps.

| Setting                        | Recommendation      |
| ---                            | ---                 |
| Bankroll                       | $1,000+             |
| % Size for each trade          | 3–6%                |
| Max % per trade                | 40–70%              |
| Exit mode                      | Mirror or Proportional |
| Max amount per market          | $200–$300           |
| Min volume of each market      | $10,000+            |
| Max time until resolution      | 90–240 days         |

### 14. Late-Event Momentum Closer

Trades in the final days when probabilities converge.

Risks: Edge is smaller but risk of shock still exists.

Result: High-confidence endgame positioning without over-betting.

| Setting                        | Recommendation      |
| ---                            | ---                 |
| Bankroll                       | $500–$1,000         |
| % Size for each trade          | 4–8%                |
| Max % per trade                | 50–100%             |
| Exit mode                      | Mirror              |
| Max amount per market          | $200–$250           |
| Min volume of each market      | $25,000+            |
| Market price range             | Min 60¢, Max 95¢    |
| Max slippage per market        | 2–4¢                |
| Max time until resolution      | 2–14 days           |

### 15. News-Scraper / Alert Trader

Scrapes feeds and alerts for very fast reactions.

Risks: If you get filled after the move, you're dead.

Result: Only trades where you actually get close to the trader's fill.

| Setting                        | Recommendation      |
| ---                            | ---                 |
| Bankroll                       | $300–$800           |
| % Size for each trade          | 2–4%                |
| Max % per trade                | 20–50%              |
| Exit mode                      | Proportional        |
| Max copy amount per trade      | $20–$40             |
| Min liquidity per market       | $5,000+             |
| Max slippage per market        | 1–2¢                |
| Max time until resolution      | 7–30 days           |

### 16. Volatility Breakout