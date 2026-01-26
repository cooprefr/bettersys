import React from 'react';
import type { TrustLevel, Disclaimer } from '../../types/backtest';
import { TrustStatusBadge } from './TrustStatusBadge';
import { DisclaimersBlock } from './DisclaimersBlock';
import { ManifestLink } from './ManifestLink';

interface TrustSignalingProps {
  trustLevel: TrustLevel;
  disclaimers: Disclaimer[];
  manifestUrl: string;
  runId?: string;
  className?: string;
}

export const TrustSignaling: React.FC<TrustSignalingProps> = ({
  trustLevel,
  disclaimers,
  manifestUrl,
  runId,
  className,
}) => {
  const isTrusted = trustLevel === 'Trusted';

  return (
    <div
      className={`trust-signaling space-y-4 ${className ?? ''}`}
      data-testid="trust-signaling"
      data-trust-level={trustLevel}
    >
      <div className="trust-header flex flex-wrap items-center justify-between gap-4">
        <TrustStatusBadge trustLevel={trustLevel} />
        <ManifestLink manifestUrl={manifestUrl} runId={runId} />
      </div>

      {!isTrusted && disclaimers.length > 0 && (
        <DisclaimersBlock disclaimers={disclaimers} />
      )}
    </div>
  );
};

interface TrustGatedContentProps {
  trustLevel: TrustLevel | undefined;
  disclaimers: Disclaimer[] | undefined;
  manifestUrl: string | undefined;
  runId?: string;
  children: React.ReactNode;
}

export const TrustGatedContent: React.FC<TrustGatedContentProps> = ({
  trustLevel,
  disclaimers,
  manifestUrl,
  runId,
  children,
}) => {
  const hasTrustData = trustLevel !== undefined && manifestUrl !== undefined;

  if (!hasTrustData) {
    return (
      <div
        className="trust-error-state bg-danger/10 border-2 border-danger p-6 text-center"
        data-testid="trust-error-state"
      >
        <div className="text-danger text-lg mb-2" aria-hidden="true">
          ✗
        </div>
        <h3 className="text-danger font-mono text-sm tracking-widest mb-2">
          TRUST DATA MISSING
        </h3>
        <p className="text-fg/80 font-mono text-xs max-w-md mx-auto">
          Cannot display performance metrics without trust status.
          The backend response is missing required trust_level or manifest_url fields.
        </p>
      </div>
    );
  }

  const effectiveTrustLevel = trustLevel;
  const effectiveDisclaimers = disclaimers ?? [];

  return (
    <div className="trust-gated-content" data-trust-level={effectiveTrustLevel}>
      <TrustSignaling
        trustLevel={effectiveTrustLevel}
        disclaimers={effectiveDisclaimers}
        manifestUrl={manifestUrl}
        runId={runId}
        className="mb-6"
      />
      {children}
    </div>
  );
};
