import { formatDistanceToNow } from 'date-fns';
import { SignalTypeVariant } from '../types/signal';

export function getSignalLabel(type: SignalTypeVariant): string {
  const labels: Record<SignalTypeVariant, string> = {
    WhaleFollowing: 'WHALE',
    TrackedWalletEntry: 'TRACKED',
    EliteWallet: 'ELITE',
    InsiderWallet: 'INSIDER',
    PriceDeviation: 'DEVIATION',
    WhaleCluster: 'CLUSTER',
    CrossPlatformArbitrage: 'ARB',
    MarketExpiryEdge: 'EXPIRY',
  };
  return labels[type] || 'SIGNAL';
}

export function formatConfidence(confidence: number): string {
  return `${(confidence * 100).toFixed(1)}%`;
}

export function formatPrice(price: number): string {
  // Show 3 decimal places only if price is less than 1 cent ($0.01)
  if (price < 0.01) {
    return `$${price.toFixed(3)}`;
  }
  return `$${price.toFixed(2)}`;
}

export function formatVolume(volume: number): string {
  // Always show exact amounts with appropriate decimal places
  if (volume >= 1) {
    // For values >= $1, show with 2 decimal places and comma separators
    return `$${volume.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
  }
  // For values < $1, show more precision (up to 4 decimal places)
  return `$${volume.toFixed(4)}`;
}

export function cleanMarketName(marketSlug: string): string {
  // Remove duplicates and clean up dashes
  const parts = marketSlug.split('-').filter(Boolean);
  const uniqueParts = [...new Set(parts)];
  
  // Convert to proper case and join with spaces
  return uniqueParts
    .map(part => part.charAt(0).toUpperCase() + part.slice(1).toLowerCase())
    .join(' ');
}

export function formatTimeAgo(timestamp: string): string {
  try {
    return formatDistanceToNow(new Date(timestamp), { addSuffix: true });
  } catch {
    return 'unknown';
  }
}

export function formatTimestamp(timestamp: string): string {
  try {
    const date = new Date(timestamp);
    return date.toLocaleTimeString('en-US', {
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
      hour12: false,
    });
  } catch {
    return timestamp;
  }
}

export function formatPnL(value: number): string {
  const sign = value >= 0 ? '+' : '';
  if (Math.abs(value) >= 1000000) {
    return `${sign}$${(value / 1000000).toFixed(1)}M`;
  }
  if (Math.abs(value) >= 1000) {
    return `${sign}$${(value / 1000).toFixed(1)}K`;
  }
  return `${sign}$${value.toFixed(0)}`;
}

export function formatVolumeCompact(volume: number): string {
  if (volume >= 1000000) {
    return `$${(volume / 1000000).toFixed(1)}M`;
  }
  if (volume >= 1000) {
    return `$${(volume / 1000).toFixed(1)}K`;
  }
  return `$${volume.toFixed(0)}`;
}

export function metricColorClass(value: number | null | undefined): string {
  if (typeof value !== 'number' || !Number.isFinite(value)) return 'text-white';
  if (value > 0) return 'text-success';
  if (value < 0) return 'text-danger';
  return 'text-grey/60';
}

function formatSignedNumber(value: number, digits: number): string {
  const sign = value >= 0 ? '+' : '';
  return `${sign}${value.toFixed(digits)}`;
}

export function formatDelta(priceDeltaBps?: number, priceDeltaAbsUsd?: number): string {
  const hasBps = typeof priceDeltaBps === 'number' && Number.isFinite(priceDeltaBps);
  const hasAbs = typeof priceDeltaAbsUsd === 'number' && Number.isFinite(priceDeltaAbsUsd);

  if (!hasBps && !hasAbs) return '---';

  const parts: string[] = [];
  if (hasBps) parts.push(`${formatSignedNumber(priceDeltaBps!, 0)}bps`);
  if (hasAbs) {
    const sign = priceDeltaAbsUsd! >= 0 ? '+' : '';
    parts.push(`${sign}${formatPrice(Math.abs(priceDeltaAbsUsd!))}`);
  }

  return parts.join(' / ');
}
