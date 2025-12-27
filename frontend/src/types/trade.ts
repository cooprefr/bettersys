export type TradeSide = 'BUY' | 'SELL';
export type TradeOrderType = 'GTC' | 'FAK' | 'FOK';
export type TradePriceMode = 'JOIN' | 'CROSS' | 'CUSTOM';

export interface TradeOrderRequest {
  signal_id?: string;
  market_slug?: string;
  outcome?: string;
  side: TradeSide;
  notional_usd: number;
  order_type: TradeOrderType;
  price_mode: TradePriceMode;
  limit_price?: number;
}

export interface TradeOrderResponse {
  ok: boolean;
  trading_enabled: boolean;
  mode?: 'paper' | 'live';
  message: string;
  request_id?: string;
}
