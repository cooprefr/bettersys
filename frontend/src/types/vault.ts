export interface VaultStateResponse {
  cash_usdc: number;
  nav_usdc: number;
  total_shares: number;
  nav_per_share: number;
}

export interface VaultOverviewResponse {
  fetched_at: number;
  engine_enabled: boolean;
  paper: boolean;
  cash_usdc: number;
  nav_usdc: number;
  total_shares: number;
  nav_per_share: number;
  wallet_address?: string;
  user_shares?: number;
  user_value_usdc?: number;
  // Real Polymarket account data (only in live mode)
  polymarket_balance?: number;
  polymarket_positions_value?: number;
  polymarket_total_value?: number;
}

export interface VaultNavPoint {
  ts: number;
  nav_per_share: number;
  nav_usdc: number;
  cash_usdc: number;
  positions_value_usdc: number;
  total_shares: number;
  source: string;
}

export interface VaultPerformanceResponse {
  fetched_at: number;
  range: string;
  points: VaultNavPoint[];
}

export interface VaultPositionResponse {
  token_id: string;
  outcome: string;
  shares: number;
  avg_price: number;
  cost_usdc: number;
  market_slug?: string;
  market_question?: string;
  end_date_iso?: string;
  tte_sec?: number;
  strategy?: string;
  decision_id?: string;
  best_bid?: number;
  best_ask?: number;
  mid?: number;
  spread?: number;
  value_usdc?: number;
  pnl_unrealized_usdc?: number;
}

export interface VaultPositionsResponse {
  fetched_at: number;
  positions: VaultPositionResponse[];
}

export interface VaultActivityRecord {
  id: string;
  ts: number;
  kind: string;
  wallet_address?: string;
  amount_usdc?: number;
  shares?: number;
  token_id?: string;
  market_slug?: string;
  outcome?: string;
  side?: string;
  price?: number;
  notional_usdc?: number;
  strategy?: string;
  decision_id?: string;
}

export interface VaultActivityResponse {
  fetched_at: number;
  events: VaultActivityRecord[];
}

export interface VaultLlmUsageStats {
  day_start_ts: number;
  calls_today: number;
  tokens_today: number;
  per_market_calls_today: Array<[string, number]>;
}

export interface VaultConfigResponse {
  fetched_at: number;
  engine_enabled: boolean;
  paper: boolean;

  updown_poll_ms: number;
  updown_min_edge: number;
  updown_kelly_fraction: number;
  updown_max_position_pct: number;
  updown_shrink_to_half: number;
  updown_cooldown_sec: number;

  long_enabled: boolean;
  long_poll_ms: number;
  long_min_edge: number;
  long_kelly_fraction: number;
  long_max_position_pct: number;
  long_min_trade_usd: number;
  long_max_trade_usd: number;
  long_min_infer_interval_sec: number;
  long_cooldown_sec: number;
  long_max_calls_per_day: number;
  long_max_calls_per_market_per_day: number;
  long_max_tokens_per_day: number;
  long_llm_timeout_sec: number;
  long_llm_max_tokens: number;
  long_llm_temperature: number;
  long_max_tte_days: number;
  long_max_spread_bps: number;
  long_min_top_of_book_usd: number;
  long_fee_buffer: number;
  long_slippage_buffer_min: number;
  long_dispersion_max: number;
  long_exit_price_90: number;
  long_exit_price_95: number;
  long_exit_frac_90: number;
  long_exit_frac_95: number;
  long_wallet_window_sec: number;
  long_wallet_max_trades_per_window: number;
  long_wallet_min_notional_usd: number;
  long_models: string[];

  llm_usage_today?: VaultLlmUsageStats;
}

export interface VaultLlmDecisionRow {
  decision_id: string;
  market_slug: string;
  created_at: number;
  action: string;
  outcome_index?: number;
  outcome_text?: string;
  p_true?: number;
  bid?: number;
  ask?: number;
  p_eff?: number;
  edge?: number;
  size_mult?: number;
  consensus_models?: string;
  flags?: string;
  rationale_hash?: string;
}

export interface VaultLlmDecisionsResponse {
  fetched_at: number;
  decisions: VaultLlmDecisionRow[];
}

export interface VaultLlmModelRecordRow {
  id: string;
  decision_id: string;
  model: string;
  created_at: number;
  parsed_ok: boolean;
  action?: string;
  outcome_index?: number;
  p_true?: number;
  uncertainty?: string;
  size_mult?: number;
  flags?: string;
  rationale_hash?: string;
  raw_dsl?: string;
  latency_ms?: number;
  prompt_tokens?: number;
  completion_tokens?: number;
  total_tokens?: number;
  error?: string;
}

export interface VaultLlmModelsResponse {
  fetched_at: number;
  records: VaultLlmModelRecordRow[];
}

export interface VaultDepositRequest {
  wallet_address: string;
  amount_usdc: number;
}

export interface VaultDepositResponse {
  wallet_address: string;
  amount_usdc: number;
  shares_minted: number;
  nav_per_share: number;
  total_shares: number;
  nav_usdc: number;
}

export interface VaultWithdrawRequest {
  wallet_address: string;
  shares: number;
}

export interface VaultWithdrawResponse {
  wallet_address: string;
  shares_burned: number;
  amount_usdc: number;
  nav_per_share: number;
  total_shares: number;
  nav_usdc: number;
}

// Backtest types
export interface BacktestPnlPoint {
  ts: number;
  equity: number;
  pnl_cumulative: number;
  drawdown: number;
}

export interface BacktestTradeRecord {
  ts: number;
  market_slug: string;
  outcome: string;
  side: string;
  entry_price: number;
  exit_price: number;
  shares: number;
  pnl: number;
  edge: number;
}

export interface BacktestResults {
  fetched_at: number;
  asset: string;
  date_range: { start: string; end: string };
  config: {
    bankroll: number;
    min_edge: number;
    kelly_fraction: number;
    max_position_pct: number;
    fee_rate: number;
  };
  summary: {
    total_orders: number;
    opportunities: number;
    trades_taken: number;
    total_volume: number;
    total_fees: number;
    realized_pnl: number;
    gross_profit: number;
    gross_loss: number;
    wins: number;
    losses: number;
    win_rate: number;
    profit_factor: number;
    max_drawdown: number;
    avg_edge: number;
    roi_pct: number;
    avg_pnl_per_trade: number;
    avg_trade_size: number;
  };
  pnl_curve: BacktestPnlPoint[];
  recent_trades: BacktestTradeRecord[];
}

// Paper Trading types
export interface PaperTradingSummary {
  signals_seen: number;
  opportunities: number;
  trades_taken: number;
  total_volume: number;
  total_fees: number;
  realized_pnl: number;
  gross_profit: number;
  gross_loss: number;
  wins: number;
  losses: number;
  win_rate: number;
  profit_factor: number;
  max_drawdown: number;
  avg_edge: number;
  roi_pct: number;
  avg_pnl_per_trade: number;
  avg_trade_size: number;
}

export interface PaperTradeRecord {
  ts: number;
  market_slug: string;
  outcome: string;
  side: string;
  entry_price: number;
  exit_price: number;
  shares: number;
  pnl: number;
  edge: number;
}

export interface PaperTradingState {
  fetched_at: number;
  is_running: boolean;
  started_at: number | null;
  uptime_secs: number;
  asset: string;
  config: {
    bankroll: number;
    min_edge: number;
    kelly_fraction: number;
    max_position_pct: number;
    fee_rate: number;
  };
  summary: PaperTradingSummary;
  pnl_curve: BacktestPnlPoint[];
  recent_trades: PaperTradeRecord[];
}

export interface PaperTradingStartRequest {
  asset: string;
  bankroll: number;
  min_edge: number;
  kelly_fraction: number;
  max_position_pct: number;
}
