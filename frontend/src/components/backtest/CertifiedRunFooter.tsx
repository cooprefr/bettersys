import React, { useState, useCallback, useMemo } from 'react';
import { copyToClipboard } from '../../hooks/useRouter';

// =============================================================================
// TYPES
// =============================================================================

export interface CertifiedRunFooterProps {
  runId: string;
  publishTimestamp: number; // Unix timestamp (seconds)
  schemaVersion: string;
  manifestHash: string;
}

export interface CertifiedRunFooterData {
  run_id: string | null | undefined;
  publish_timestamp: number | null | undefined;
  schema_version: string | null | undefined;
  manifest_hash: string | null | undefined;
}

// =============================================================================
// VALIDATION
// =============================================================================

export interface FooterValidationResult {
  isValid: boolean;
  missingFields: string[];
}

/**
 * Validates that all required footer fields are present and non-null.
 * Returns validation result with list of missing fields.
 */
export function validateFooterData(data: CertifiedRunFooterData): FooterValidationResult {
  const missingFields: string[] = [];

  if (!data.run_id || typeof data.run_id !== 'string' || data.run_id.trim() === '') {
    missingFields.push('run_id');
  }

  if (data.publish_timestamp === null || data.publish_timestamp === undefined || typeof data.publish_timestamp !== 'number') {
    missingFields.push('publish_timestamp');
  }

  if (!data.schema_version || typeof data.schema_version !== 'string' || data.schema_version.trim() === '') {
    missingFields.push('schema_version');
  }

  if (!data.manifest_hash || typeof data.manifest_hash !== 'string' || data.manifest_hash.trim() === '') {
    missingFields.push('manifest_hash');
  }

  return {
    isValid: missingFields.length === 0,
    missingFields,
  };
}

// =============================================================================
// HARD ERROR STATE COMPONENT
// =============================================================================

export interface FooterMissingFieldsErrorProps {
  missingFields: string[];
}

/**
 * Hard error state when required footer fields are missing.
 * This component blocks the entire page from rendering results.
 */
export const FooterMissingFieldsError: React.FC<FooterMissingFieldsErrorProps> = ({ missingFields }) => (
  <div
    role="alert"
    aria-live="assertive"
    className="min-h-screen bg-void text-fg flex items-center justify-center p-6"
  >
    <div className="bg-danger/10 border-2 border-danger p-8 max-w-lg w-full">
      <div className="text-danger text-3xl mb-4" aria-hidden="true">
        ✗
      </div>
      <h1 className="text-danger font-mono text-xl tracking-widest mb-4">
        ARTIFACT PROVENANCE ERROR
      </h1>
      <p className="text-fg/90 font-mono text-sm mb-6">
        This run cannot be displayed because required provenance fields are missing from the API response.
        The UI cannot render partial or unverifiable results.
      </p>
      <div className="bg-void/50 border border-danger/40 p-4 mb-6">
        <div className="text-[11px] font-mono text-danger/80 tracking-widest mb-2">
          MISSING REQUIRED FIELDS:
        </div>
        <ul className="list-none space-y-1">
          {missingFields.map((field) => (
            <li key={field} className="text-sm font-mono text-fg">
              <span className="text-danger mr-2">•</span>
              {field}
            </li>
          ))}
        </ul>
      </div>
      <p className="text-fg/60 font-mono text-xs">
        Contact the system administrator if this error persists.
        Run artifacts must include all provenance metadata to ensure integrity.
      </p>
    </div>
  </div>
);

// =============================================================================
// HELPERS
// =============================================================================

/**
 * Truncates a hash for display, showing first N characters.
 */
function truncateHash(hash: string, length: number = 12): string {
  if (!hash || hash.length <= length) return hash || '---';
  return hash.slice(0, length);
}

/**
 * Formats a Unix timestamp as ISO-8601 UTC string.
 */
function formatTimestampUTC(timestamp: number): string {
  try {
    const date = new Date(timestamp * 1000);
    return date.toISOString().replace('T', ' ').replace('Z', ' UTC');
  } catch {
    return '---';
  }
}

// =============================================================================
// HASH DISPLAY COMPONENT
// =============================================================================

interface ManifestHashDisplayProps {
  hash: string;
}

const ManifestHashDisplay: React.FC<ManifestHashDisplayProps> = ({ hash }) => {
  const [showFull, setShowFull] = useState(false);
  const [copied, setCopied] = useState(false);

  const handleCopy = useCallback(async () => {
    const success = await copyToClipboard(hash);
    if (success) {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  }, [hash]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        setShowFull((prev) => !prev);
      }
    },
    []
  );

  const shortHash = useMemo(() => truncateHash(hash, 12), [hash]);

  return (
    <span className="inline-flex items-center gap-2">
      <span
        role="button"
        tabIndex={0}
        aria-label={`Manifest Hash: ${shortHash}. Click or press Enter to ${showFull ? 'hide' : 'show'} full hash.`}
        aria-expanded={showFull}
        onClick={() => setShowFull((prev) => !prev)}
        onKeyDown={handleKeyDown}
        onMouseEnter={() => setShowFull(true)}
        onMouseLeave={() => setShowFull(false)}
        className="cursor-pointer hover:text-fg transition-colors underline decoration-dotted underline-offset-2"
        title={showFull ? hash : 'Click to show full hash'}
      >
        {showFull ? hash : shortHash}
      </span>
      <button
        onClick={handleCopy}
        aria-label={copied ? 'Copied to clipboard' : 'Copy manifest hash to clipboard'}
        className="text-[9px] px-1.5 py-0.5 border border-grey/30 text-fg/60 hover:text-fg hover:border-grey/50 transition-colors font-mono"
      >
        {copied ? '✓' : 'COPY'}
      </button>
    </span>
  );
};

// =============================================================================
// MAIN FOOTER COMPONENT
// =============================================================================

/**
 * CertifiedRunFooter - Mandatory provenance footer for backtest run pages.
 *
 * This footer MUST be rendered on every run detail page as an anti-tamper
 * and provenance signal. It displays:
 * - Run ID
 * - Publish Timestamp (UTC)
 * - Schema Version
 * - Manifest Hash (with hover/click for full hash and copy)
 *
 * The footer is:
 * - Always visible (not dismissible)
 * - Read-only (values derived from certified run artifact)
 * - High-contrast and screen-reader accessible
 */
export const CertifiedRunFooter: React.FC<CertifiedRunFooterProps> = ({
  runId,
  publishTimestamp,
  schemaVersion,
  manifestHash,
}) => {
  const formattedTimestamp = useMemo(
    () => formatTimestampUTC(publishTimestamp),
    [publishTimestamp]
  );

  return (
    <footer
      role="contentinfo"
      aria-label="Certified run provenance information"
      className="fixed bottom-0 left-0 right-0 z-50 bg-void border-t border-grey/20 px-4 py-3"
      style={{ minHeight: '48px' }}
      data-testid="certified-run-footer"
    >
      <div className="max-w-6xl mx-auto">
        {/* Desktop layout */}
        <div className="hidden md:flex items-center justify-between gap-6 text-[10px] font-mono">
          <div className="flex items-center gap-6">
            <div className="flex items-center gap-2">
              <span className="text-fg/50">Run ID:</span>
              <span className="text-fg/90" data-testid="footer-run-id">
                {runId}
              </span>
            </div>
            <div className="flex items-center gap-2">
              <span className="text-fg/50">Published (UTC):</span>
              <span className="text-fg/90" data-testid="footer-publish-timestamp">
                {formattedTimestamp}
              </span>
            </div>
            <div className="flex items-center gap-2">
              <span className="text-fg/50">Schema Version:</span>
              <span className="text-fg/90" data-testid="footer-schema-version">
                {schemaVersion}
              </span>
            </div>
          </div>
          <div className="flex items-center gap-2">
            <span className="text-fg/50">Manifest Hash:</span>
            <span className="text-fg/90" data-testid="footer-manifest-hash">
              <ManifestHashDisplay hash={manifestHash} />
            </span>
          </div>
        </div>

        {/* Mobile layout */}
        <div className="md:hidden text-[9px] font-mono space-y-1.5">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-1.5">
              <span className="text-fg/50">Run:</span>
              <span className="text-fg/90 truncate max-w-[120px]" data-testid="footer-run-id-mobile">
                {runId}
              </span>
            </div>
            <div className="flex items-center gap-1.5">
              <span className="text-fg/50">Schema:</span>
              <span className="text-fg/90" data-testid="footer-schema-version-mobile">
                {schemaVersion}
              </span>
            </div>
          </div>
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-1.5">
              <span className="text-fg/50">UTC:</span>
              <span className="text-fg/90 truncate" data-testid="footer-publish-timestamp-mobile">
                {formattedTimestamp}
              </span>
            </div>
            <div className="flex items-center gap-1.5">
              <span className="text-fg/50">Hash:</span>
              <span className="text-fg/90" data-testid="footer-manifest-hash-mobile">
                <ManifestHashDisplay hash={manifestHash} />
              </span>
            </div>
          </div>
        </div>
      </div>
    </footer>
  );
};

export default CertifiedRunFooter;
