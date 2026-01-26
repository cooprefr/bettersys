import React from 'react';
import type { Disclaimer, DisclaimerSeverity } from '../../types/backtest';

interface DisclaimersBlockProps {
  disclaimers: Disclaimer[];
  className?: string;
}

const SEVERITY_CONFIG: Record<DisclaimerSeverity, { icon: string; className: string }> = {
  Critical: {
    icon: '✗',
    className: 'border-danger/50 bg-danger/5 text-danger',
  },
  Warning: {
    icon: '⚠',
    className: 'border-warning/50 bg-warning/5 text-warning',
  },
  Info: {
    icon: 'ℹ',
    className: 'border-grey/30 bg-grey/5 text-fg/80',
  },
};

const DisclaimerItem: React.FC<{ disclaimer: Disclaimer }> = ({ disclaimer }) => {
  const config = SEVERITY_CONFIG[disclaimer.severity] ?? SEVERITY_CONFIG.Info;

  return (
    <div
      className={`disclaimer-item border-l-4 p-3 font-mono text-xs ${config.className}`}
      data-disclaimer-id={disclaimer.id}
      data-disclaimer-severity={disclaimer.severity}
    >
      <div className="flex items-start gap-2">
        <span className="disclaimer-icon flex-shrink-0 text-sm" aria-hidden="true">
          {config.icon}
        </span>
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2 mb-1">
            <span className="disclaimer-severity font-semibold tracking-widest">
              [{disclaimer.severity.toUpperCase()}]
            </span>
            <span className="disclaimer-category text-fg/60 tracking-wider">
              {disclaimer.category}
            </span>
          </div>
          <p className="disclaimer-message whitespace-pre-wrap break-words">
            {disclaimer.message}
          </p>
          {disclaimer.evidence.length > 0 && (
            <ul className="disclaimer-evidence mt-2 space-y-0.5 text-fg/60">
              {disclaimer.evidence.map((ev, i) => (
                <li key={i} className="flex items-start gap-1">
                  <span className="flex-shrink-0">•</span>
                  <span className="break-all">{ev}</span>
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>
    </div>
  );
};

export const DisclaimersBlock: React.FC<DisclaimersBlockProps> = ({
  disclaimers,
  className,
}) => {
  if (disclaimers.length === 0) {
    return null;
  }

  const criticalCount = disclaimers.filter((d) => d.severity === 'Critical').length;
  const warningCount = disclaimers.filter((d) => d.severity === 'Warning').length;

  return (
    <div
      className={`disclaimers-block bg-surface border border-grey/20 ${className ?? ''}`}
      data-testid="disclaimers-block"
      data-critical-count={criticalCount}
      data-warning-count={warningCount}
    >
      <div className="disclaimers-header flex items-center justify-between px-4 py-3 border-b border-grey/20 bg-danger/5">
        <div className="flex items-center gap-2">
          <span className="text-danger text-lg" aria-hidden="true">
            ⚠
          </span>
          <span className="text-xs font-mono tracking-widest text-danger font-semibold">
            DISCLAIMERS ({disclaimers.length})
          </span>
        </div>
        <div className="flex items-center gap-3 text-[10px] font-mono tracking-wider">
          {criticalCount > 0 && (
            <span className="text-danger">
              {criticalCount} CRITICAL
            </span>
          )}
          {warningCount > 0 && (
            <span className="text-warning">
              {warningCount} WARNING
            </span>
          )}
        </div>
      </div>
      <div className="disclaimers-content p-4 space-y-3">
        {disclaimers.map((d) => (
          <DisclaimerItem key={d.id} disclaimer={d} />
        ))}
      </div>
    </div>
  );
};
