import React from 'react';
import type { ArtifactMeta } from '../../services/certifiedRunCache';
import { formatManifestHash, formatPublishTimestamp } from '../../services/certifiedRunCache';

interface CertifiedArtifactIndicatorProps {
  meta: ArtifactMeta | null;
  className?: string;
  /** If true, show a more compact version */
  compact?: boolean;
}

/**
 * CertifiedArtifactIndicator
 * 
 * Displays the certified artifact confirmation:
 *   "Certified run · Manifest hash: abc123… · Published: 2026-01-24 14:03 UTC"
 * 
 * This indicator is driven solely by backend-provided fields, not client state.
 * It updates if and only if the fetched artifact is different.
 */
export const CertifiedArtifactIndicator: React.FC<CertifiedArtifactIndicatorProps> = ({
  meta,
  className,
  compact = false,
}) => {
  if (!meta) {
    return null;
  }

  const hashDisplay = formatManifestHash(meta.manifestHash);
  const timestampDisplay = formatPublishTimestamp(meta.publishTimestamp);
  const isTrusted = meta.trustLevel === 'Trusted';

  if (compact) {
    return (
      <div
        className={`certified-artifact-indicator inline-flex items-center gap-2 text-[10px] font-mono text-fg/60 ${className ?? ''}`}
        data-testid="certified-artifact-indicator"
        data-manifest-hash={meta.manifestHash}
        data-trust-level={meta.trustLevel}
      >
        <span className={isTrusted ? 'text-success' : 'text-warning'}>●</span>
        <span className="tracking-wider">
          {hashDisplay !== '---' ? `#${hashDisplay}` : '---'}
        </span>
      </div>
    );
  }

  return (
    <div
      className={`certified-artifact-indicator flex flex-wrap items-center gap-x-3 gap-y-1 text-[10px] font-mono text-fg/60 ${className ?? ''}`}
      data-testid="certified-artifact-indicator"
      data-manifest-hash={meta.manifestHash}
      data-trust-level={meta.trustLevel}
    >
      <span className="flex items-center gap-1.5">
        <span className={isTrusted ? 'text-success' : 'text-warning'}>●</span>
        <span className="tracking-widest">CERTIFIED RUN</span>
      </span>
      
      <span className="text-fg/30">·</span>
      
      <span className="tracking-wider">
        MANIFEST: <span className="text-fg/80">{hashDisplay}</span>
      </span>
      
      <span className="text-fg/30">·</span>
      
      <span className="tracking-wider">
        PUBLISHED: <span className="text-fg/80">{timestampDisplay}</span>
      </span>

      {meta.etag && (
        <>
          <span className="text-fg/30">·</span>
          <span className="tracking-wider text-fg/40" title={`ETag: ${meta.etag}`}>
            ETAG: {meta.etag.slice(0, 8)}…
          </span>
        </>
      )}
    </div>
  );
};

/**
 * CertifiedArtifactFooter
 * 
 * A footer-style variant of the indicator for use at the bottom of pages.
 */
export const CertifiedArtifactFooter: React.FC<CertifiedArtifactIndicatorProps> = ({
  meta,
  className,
}) => {
  if (!meta) {
    return null;
  }

  const hashDisplay = formatManifestHash(meta.manifestHash);
  const timestampDisplay = formatPublishTimestamp(meta.publishTimestamp);
  const isTrusted = meta.trustLevel === 'Trusted';

  return (
    <div
      className={`certified-artifact-footer border-t border-grey/10 py-3 px-4 bg-surface/50 ${className ?? ''}`}
      data-testid="certified-artifact-footer"
      data-manifest-hash={meta.manifestHash}
    >
      <div className="flex flex-wrap items-center justify-between gap-2 text-[10px] font-mono">
        <div className="flex items-center gap-2 text-fg/60">
          <span className={isTrusted ? 'text-success' : 'text-warning'}>●</span>
          <span className="tracking-widest">IMMUTABLE CERTIFIED ARTIFACT</span>
        </div>
        
        <div className="flex items-center gap-3 text-fg/50">
          <span>
            HASH: <span className="text-fg/70">{hashDisplay}</span>
          </span>
          <span className="text-fg/20">|</span>
          <span>
            {timestampDisplay}
          </span>
        </div>
      </div>
    </div>
  );
};

/**
 * CacheStatusBadge
 * 
 * Shows whether data came from cache (for debugging/transparency).
 */
export const CacheStatusBadge: React.FC<{
  fromCache: boolean;
  notModified: boolean;
  className?: string;
}> = ({ fromCache, notModified, className }) => {
  if (!fromCache && !notModified) {
    return null;
  }

  return (
    <span
      className={`inline-flex items-center gap-1 px-2 py-0.5 text-[9px] font-mono tracking-widest border ${
        notModified
          ? 'border-success/30 text-success/70 bg-success/5'
          : 'border-grey/30 text-fg/50 bg-grey/5'
      } ${className ?? ''}`}
      title={
        notModified
          ? 'Server returned 304 Not Modified - data unchanged'
          : 'Data loaded from local cache'
      }
    >
      {notModified ? '304' : 'CACHED'}
    </span>
  );
};
