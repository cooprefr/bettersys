import React, { useState, useCallback, useMemo } from 'react';
import type { ProvenanceSummary, TrustLevel, StrategyId, RunFingerprint } from '../../types/backtest';
import { api } from '../../services/api';

// =============================================================================
// TYPES
// =============================================================================

interface ProvenancePanelProps {
  runId: string;
  provenance: ProvenanceSummary;
  trustLevel: TrustLevel;
  trustReason?: string;
  manifestUrl?: string;
  productionGrade?: boolean;
  className?: string;
}

// =============================================================================
// CONSTANTS
// =============================================================================

const DATASET_READINESS_TOOLTIPS: Record<string, string> = {
  TakerOnly: 'Dataset supports taker (aggressive) execution only. Maker fills cannot be reliably simulated.',
  MakerViable: 'Dataset includes queue position data suitable for maker (passive) execution simulation.',
  NonRepresentative: 'Dataset lacks sufficient fidelity for production-grade backtesting.',
};

const SETTLEMENT_SOURCE_LABELS: Record<string, string> = {
  ChainlinkPolygon: 'Chainlink (Polygon)',
  Chainlink: 'Chainlink',
  Simulated: 'Simulated',
  ExactSpec: 'Exact Settlement Spec',
};

// =============================================================================
// TOOLTIP COMPONENT
// =============================================================================

const Tooltip: React.FC<{ text: string; children: React.ReactNode }> = ({ text, children }) => {
  const [visible, setVisible] = useState(false);

  return (
    <span
      className="relative inline-flex items-center cursor-help"
      onMouseEnter={() => setVisible(true)}
      onMouseLeave={() => setVisible(false)}
    >
      {children}
      {visible && (
        <span className="absolute z-50 left-full ml-2 top-1/2 -translate-y-1/2 px-2 py-1 bg-bg border border-grey/30 text-[10px] font-mono text-fg/80 whitespace-nowrap max-w-xs">
          {text}
        </span>
      )}
    </span>
  );
};

// =============================================================================
// FIELD ROW COMPONENT
// =============================================================================

const FieldRow: React.FC<{
  label: string;
  value: string | React.ReactNode;
  tooltip?: string;
  mono?: boolean;
}> = ({ label, value, tooltip, mono = false }) => (
  <div className="flex items-start gap-3 py-1.5">
    <span className="text-[10px] text-fg/60 tracking-widest w-40 flex-shrink-0 uppercase">
      {label}
      {tooltip && (
        <Tooltip text={tooltip}>
          <span className="ml-1 text-fg/40 cursor-help">[?]</span>
        </Tooltip>
      )}
    </span>
    <span className={`text-[11px] text-fg/90 ${mono ? 'font-mono' : ''}`}>
      {value || '---'}
    </span>
  </div>
);

// =============================================================================
// SECTION HEADER COMPONENT
// =============================================================================

const SectionHeader: React.FC<{ title: string }> = ({ title }) => (
  <div className="text-[9px] text-fg/50 tracking-[0.2em] uppercase mt-4 mb-2 pb-1 border-b border-grey/10">
    {title}
  </div>
);

// =============================================================================
// DOWNLOAD BUTTON COMPONENT
// =============================================================================

const DownloadManifestButton: React.FC<{
  manifestUrl: string;
  runId: string;
}> = ({ manifestUrl, runId }) => {
  const [downloading, setDownloading] = useState(false);

  const handleDownload = useCallback(async () => {
    if (downloading) return;
    setDownloading(true);

    try {
      const response = await fetch(manifestUrl);
      if (!response.ok) {
        throw new Error(`Failed to fetch manifest: ${response.status}`);
      }
      const blob = await response.blob();
      const url = URL.createObjectURL(blob);
      const a = document.createElement('a');
      a.href = url;
      a.download = `run_manifest_${runId}.json`;
      document.body.appendChild(a);
      a.click();
      document.body.removeChild(a);
      URL.revokeObjectURL(url);
    } catch (err) {
      console.error('Failed to download manifest:', err);
    } finally {
      setDownloading(false);
    }
  }, [manifestUrl, runId, downloading]);

  return (
    <button
      onClick={handleDownload}
      disabled={downloading}
      className="flex items-center gap-2 px-3 py-2 border border-accent/50 bg-accent/5 hover:bg-accent/10 text-accent font-mono text-[11px] tracking-widest transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
    >
      <span className="text-sm" aria-hidden="true">↓</span>
      <span>{downloading ? 'DOWNLOADING...' : 'DOWNLOAD RUN MANIFEST (JSON)'}</span>
    </button>
  );
};

// =============================================================================
// FORMAT HELPERS
// =============================================================================

function formatStrategyId(strategy?: StrategyId): string {
  if (!strategy) return '---';
  const hash = strategy.code_hash ? `@${strategy.code_hash.slice(0, 8)}` : '';
  return `${strategy.name} v${strategy.version}${hash}`;
}

function formatDatasetReadiness(readiness: string): { label: string; tooltip: string } {
  const normalized = readiness.replace(/([A-Z])/g, ' $1').trim();
  const tooltip = DATASET_READINESS_TOOLTIPS[readiness] || 'Unknown readiness classification.';
  return { label: normalized, tooltip };
}

function formatSettlementSource(source: string): string {
  return SETTLEMENT_SOURCE_LABELS[source] || source;
}

function formatFingerprint(fingerprint?: RunFingerprint): string {
  if (!fingerprint) return '---';
  return fingerprint.hash_hex;
}

function formatConfigFingerprint(fingerprint?: RunFingerprint): string {
  if (!fingerprint?.dataset_hash) return '---';
  return fingerprint.dataset_hash.toString(16).padStart(16, '0');
}

// =============================================================================
// MAIN COMPONENT
// =============================================================================

export const ProvenancePanel: React.FC<ProvenancePanelProps> = ({
  runId,
  provenance,
  trustLevel,
  trustReason,
  manifestUrl,
  productionGrade,
  className,
}) => {
  const [expanded, setExpanded] = useState(false);

  const datasetReadiness = formatDatasetReadiness(provenance.dataset.readiness);
  const isTrusted = trustLevel === 'Trusted';

  // Use provided manifestUrl or construct one via API
  const effectiveManifestUrl = useMemo(() => {
    return manifestUrl || api.getManifestUrl(runId);
  }, [manifestUrl, runId]);

  return (
    <div
      className={`provenance-panel border border-grey/20 ${className ?? ''}`}
      data-testid="provenance-panel"
      data-expanded={expanded}
    >
      {/* Header / Toggle */}
      <button
        onClick={() => setExpanded(!expanded)}
        className="w-full flex items-center justify-between px-4 py-3 text-left hover:bg-grey/5 transition-colors"
        aria-expanded={expanded}
        aria-controls="provenance-content"
      >
        <div className="flex items-center gap-3">
          <span className="text-[11px] font-mono text-fg/90 tracking-widest">
            PROVENANCE & REPRODUCIBILITY
          </span>
          <span className="text-[9px] text-fg/50 font-mono tracking-wider">
            (AUDIT SURFACE)
          </span>
        </div>
        <span className="text-[11px] font-mono text-fg/60">
          {expanded ? '[-]' : '[+]'}
        </span>
      </button>

      {/* Collapsible Content */}
      {expanded && (
        <div
          id="provenance-content"
          className="px-4 pb-4 border-t border-grey/10"
        >
          {/* Strategy Identity */}
          <SectionHeader title="Strategy Identity" />
          <FieldRow
            label="Strategy"
            value={formatStrategyId(provenance.strategy_id)}
            mono
          />
          {provenance.strategy_id?.version && (
            <FieldRow
              label="Version"
              value={provenance.strategy_id.version}
              mono
            />
          )}

          {/* Dataset */}
          <SectionHeader title="Dataset" />
          <FieldRow
            label="Version ID"
            value={formatConfigFingerprint(provenance.run_fingerprint)}
            mono
          />
          <FieldRow
            label="Readiness"
            value={datasetReadiness.label}
            tooltip={datasetReadiness.tooltip}
          />
          <FieldRow
            label="Classification"
            value={provenance.dataset.classification}
          />

          {/* Settlement */}
          <SectionHeader title="Settlement Reference" />
          <FieldRow
            label="Source"
            value={formatSettlementSource(provenance.settlement_source)}
          />
          <FieldRow
            label="Reference Rule"
            value={provenance.operating_mode || '---'}
            tooltip="The exact settlement reference rule used for determining window outcomes."
          />

          {/* Production Mode */}
          <SectionHeader title="Operating Mode" />
          <FieldRow
            label="Production-grade"
            value={
              <span className={productionGrade ? 'text-success' : 'text-warning'}>
                {productionGrade ? 'Yes' : 'No'}
              </span>
            }
          />

          {/* Trust Status */}
          <SectionHeader title="Trust Status" />
          <FieldRow
            label="Status"
            value={
              <span className={isTrusted ? 'text-success' : 'text-danger'}>
                {isTrusted ? 'Trusted' : 'Untrusted'}
              </span>
            }
          />
          {!isTrusted && trustReason && (
            <FieldRow
              label="Reason"
              value={
                <span className="text-danger/80">{trustReason}</span>
              }
            />
          )}

          {/* Fingerprint */}
          <SectionHeader title="Run Fingerprint" />
          <FieldRow
            label="Full Hash"
            value={formatFingerprint(provenance.run_fingerprint)}
            mono
          />
          {provenance.run_fingerprint?.seed !== undefined && (
            <FieldRow
              label="Seed"
              value={String(provenance.run_fingerprint.seed)}
              mono
            />
          )}

          {/* Download Artifact - always show since we can construct the URL */}
          <SectionHeader title="Downloadable Artifacts" />
          <div className="mt-2">
            <DownloadManifestButton manifestUrl={effectiveManifestUrl} runId={runId} />
          </div>
          <p className="mt-2 text-[9px] text-fg/50 font-mono leading-relaxed">
            The manifest contains the complete run fingerprint, config fingerprint,
            dataset fingerprint, and all metadata needed to verify reproducibility.
          </p>
        </div>
      )}
    </div>
  );
};

export default ProvenancePanel;
