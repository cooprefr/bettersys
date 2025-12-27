// Signal types matching backend Rust types
// Backend sends signal_type as a tagged enum with "type" field

export type SignalTypeVariant =
  | 'PriceDeviation'
  | 'MarketExpiryEdge'
  | 'WhaleFollowing'
  | 'EliteWallet'
  | 'InsiderWallet'
  | 'WhaleCluster'
  | 'CrossPlatformArbitrage'
  | 'TrackedWalletEntry';

export type SignalType =
  | { type: 'PriceDeviation'; market_price: number; fair_value: number; deviation_pct: number }
  | { type: 'MarketExpiryEdge'; hours_to_expiry: number; volume_spike: number }
  | { type: 'WhaleFollowing'; whale_address: string; position_size: number; confidence_score: number }
  | { type: 'EliteWallet'; wallet_address: string; win_rate: number; total_volume: number; position_size: number }
  | { type: 'InsiderWallet'; wallet_address: string; early_entry_score: number; win_rate: number; position_size: number }
  | { type: 'WhaleCluster'; cluster_count: number; total_volume: number; consensus_direction: string }
  | { type: 'CrossPlatformArbitrage'; polymarket_price: number; kalshi_price?: number; spread_pct: number }
  | { type: 'TrackedWalletEntry'; wallet_address: string; wallet_label: string; position_value_usd: number; order_count: number; token_label?: string };

export interface Signal {
  id: string;
  signal_type: SignalType;
  market_slug: string;
  confidence: number;
  detected_at: string;
  details: SignalDetails;
  source: string;

  // Optional enrichment context (pushed async after initial signal)
  context?: SignalContext;
  context_status?: string;
  context_version?: number;
  context_enriched_at?: number;
}

export interface SignalContextUpdate {
  signal_id: string;
  context_version: number;
  enriched_at: number;
  status: string;
  context: SignalContext;
}

export interface SignalContext {
  order: SignalContextOrder;
  market?: any;
  price?: SignalContextPrice;
  trade_history?: any;
  orderbook?: SignalContextOrderbook;
  activity?: any;
  candlesticks?: any;
  wallet?: any;
  wallet_pnl?: any;
  derived: SignalContextDerived;
  errors: string[];
}

export interface SignalContextOrder {
  user: string;
  market_slug: string;
  condition_id: string;
  token_id: string;
  token_label?: string;
  side: string;
  price: number;
  shares_normalized: number;
  timestamp: number;
  order_hash: string;
  tx_hash: string;
  title: string;
}

export interface SignalContextDerived {
  price_delta_abs?: number;
  price_delta_bps?: number;
  entry_value_usd?: number;
  spread_at_entry?: number;
  pnl_7d?: number;
  pnl_14d?: number;
  pnl_30d?: number;
  pnl_90d?: number;

  sharpe_7d?: number;
  sharpe_14d?: number;
  sharpe_30d?: number;
  sharpe_90d?: number;

  avg_trade_value_24h?: number;
  trade_count_24h?: number;
}

export interface MarketSnapshotLevel {
  price: number;
  size: number;
}

export interface MarketSnapshotDepth {
  bps_10: number;
  bps_25: number;
  bps_50: number;
}

export interface MarketSnapshotResponse {
  token_id: string;
  fetched_at: number;
  best_bid?: number;
  best_ask?: number;
  mid?: number;
  spread?: number;
  depth?: MarketSnapshotDepth;
  imbalance_10bps?: number;
  bids: MarketSnapshotLevel[];
  asks: MarketSnapshotLevel[];
}

export interface EquityPoint {
  timestamp: number;
  value: number;
}

export interface WalletAnalyticsResponse {
  wallet_address: string;
  updated_at: number;
  lookback_days: number;
  fixed_buy_notional_usd: number;
  wallet_realized_curve: EquityPoint[];
  copy_trade_curve: EquityPoint[];
  wallet_total_pnl?: number;
  wallet_roe_pct?: number;
  wallet_roe_denom_usd?: number;
  wallet_win_rate?: number;
  wallet_profit_factor?: number;
  copy_total_pnl?: number;
  copy_roe_pct?: number;
  copy_roe_denom_usd?: number;
  copy_win_rate?: number;
  copy_profit_factor?: number;
  copy_sharpe_7d?: number;
  copy_sharpe_14d?: number;
  copy_sharpe_30d?: number;
  copy_sharpe_90d?: number;
  // Friction modeling
  copy_friction_mode?: string;
  copy_friction_pct_per_trade?: number;
  copy_total_friction_usd?: number;
  copy_trade_count?: number;
}

export interface SignalContextPrice {
  at_entry?: MarketPriceSnapshot;
  latest?: MarketPriceSnapshot;
}

export interface MarketPriceSnapshot {
  price: number;
  at_time: number;
}

export interface SignalContextOrderbook {
  best_bid?: number;
  best_ask?: number;
  mid?: number;
  spread?: number;
  snapshot_count?: number;
}

export interface SignalDetails {
  market_id: string;
  market_title: string;
  current_price: number;
  volume_24h: number;
  liquidity?: number;
  recommended_action: string;
  position_size?: number;
  entry_price?: number;
  wallet_address?: string;
  wallet_tier?: string;
  win_rate?: number;
  spread?: number;
  expected_profit?: number;
  time_to_expiry?: string;
  dominant_side?: string;
  dominant_percentage?: number;
  expiry_time?: string | null;
  observed_timestamp?: string;
  signal_family?: string;
  calibration_version?: string;
  guardrail_flags?: string[];
  recommended_size?: number;
}

export interface CompositeSignal {
  id: string;
  pattern_type: PatternType;
  signals: Signal[];
  combined_confidence: number;
  detected_at: string;
  market_slug: string;
  recommendation: string;
}

export type PatternType =
  | 'Convergence'
  | 'Divergence'
  | 'WhaleConsensus'
  | 'VolumeAnomaly'
  | 'ArbitrageCluster';

export interface SignalStats {
  total_signals: number;
  signals_by_type: Record<string, number>;
  avg_confidence: number;
  high_confidence_count: number;
}
