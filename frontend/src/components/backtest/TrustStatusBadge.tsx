import React from 'react';
import type { TrustLevel } from '../../types/backtest';

interface TrustStatusBadgeProps {
  trustLevel: TrustLevel;
  className?: string;
}

const TRUST_CONFIG: Record<TrustLevel, { label: string; icon: string; className: string }> = {
  Trusted: {
    label: 'TRUSTED',
    icon: '✓',
    className: 'border-success/60 bg-success/10 text-success',
  },
  Untrusted: {
    label: 'UNTRUSTED',
    icon: '✗',
    className: 'border-danger/60 bg-danger/10 text-danger',
  },
  Unknown: {
    label: 'UNTRUSTED',
    icon: '?',
    className: 'border-danger/60 bg-danger/10 text-danger',
  },
  Bypassed: {
    label: 'UNTRUSTED',
    icon: '⊘',
    className: 'border-danger/60 bg-danger/10 text-danger',
  },
};

export const TrustStatusBadge: React.FC<TrustStatusBadgeProps> = ({ trustLevel, className }) => {
  const config = TRUST_CONFIG[trustLevel] ?? TRUST_CONFIG.Untrusted;

  return (
    <div
      className={`trust-status-badge inline-flex items-center gap-2 px-3 py-1.5 border-2 font-mono text-sm tracking-widest select-none ${config.className} ${className ?? ''}`}
      data-trust-level={trustLevel}
      data-testid="trust-status-badge"
    >
      <span className="trust-status-icon text-base" aria-hidden="true">
        {config.icon}
      </span>
      <span className="trust-status-label">{config.label}</span>
    </div>
  );
};
