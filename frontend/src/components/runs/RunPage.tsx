import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { api } from '../../services/api';
import { copyToClipboard } from '../../hooks/useRouter';
import type {
  AggregatedRunData,
  ManifestIntegrityError,
  TrustLevel,
  Disclaimer,
  PnLHistogramBin,
} from '../../types/backtest';
import {
  CertifiedRunFooter,
  FooterMissingFieldsError,
  validateFooterData,
  type CertifiedRunFooterData,
} from '../backtest/CertifiedRunFooter';

// =============================================================================
// FORMATTERS
// =============================================================================

function formatUsd(v: number | null | undefined, digits: number = 2): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '---';
  const sign = v < 0 ? '-' : '';
  return `${sign}$${Math.abs(v).toLocaleString('en-US', {
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  })}`;
}

function formatPct(v: number | null | undefined, digits: number = 1): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '---';
  return `${(v * 100).toFixed(digits)}%`;
}

function formatNum(v: number | null | undefined, digits: number = 2): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '---';
  return v.toFixed(digits);
}
void formatNum; // Keep for potential future use

function formatDateRange(startNs?: number, endNs?: number): string {
  if (!startNs || !endNs) return '---';
  try {
    const start = new Date(startNs / 1_000_000).toISOString().slice(0, 10);
    const end = new Date(endNs / 1_000_000).toISOString().slice(0, 10);
    return `${start} to ${end}`;
  } catch {
    return '---';
  }
}

function truncateHash(hash: string, length: number = 12): string {
  if (!hash || hash.length <= length) return hash || '---';
  return hash.slice(0, length);
}

// =============================================================================
// TRUST BADGE
// =============================================================================

const TrustBadge: React.FC<{ level: TrustLevel }> = ({ level }) => {
  const config: Record<TrustLevel, { label: string; cls: string; icon: string }> = {
    Trusted: { label: 'TRUSTED', cls: 'border-success/60 text-success bg-success/10', icon: '✓' },
    Untrusted: { label: 'UNTRUSTED', cls: 'border-danger/60 text-danger bg-danger/10', icon: '✗' },
    Unknown: { label: 'UNKNOWN', cls: 'border-grey/40 text-fg/60 bg-grey/10', icon: '?' },
    Bypassed: { label: 'BYPASSED', cls: 'border-warning/60 text-warning bg-warning/10', icon: '⊘' },
  };
  const c = config[level] || config.Unknown;
  return (
    <span className={`inline-flex items-center gap-1.5 px-3 py-1 text-[11px] font-mono border tracking-widest select-none ${c.cls}`}>
      <span>{c.icon}</span>
      <span>{c.label}</span>
    </span>
  );
};

// =============================================================================
// KPI CARD
// =============================================================================

const KpiCard: React.FC<{
  label: string;
  value: string;
  sub?: string;
  tone?: 'default' | 'success' | 'danger';
}> = ({ label, value, sub, tone = 'default' }) => {
  const valueCls = tone === 'success' ? 'text-success' : tone === 'danger' ? 'text-danger' : 'text-fg';
  return (
    <div className="bg-surface border border-grey/10 p-4">
      <div className="text-[10px] text-fg/90 tracking-widest mb-2">{label}</div>
      <div className={`text-xl font-mono ${valueCls}`}>{value}</div>
      {sub && <div className="text-[10px] font-mono text-fg/80 mt-1">{sub}</div>}
    </div>
  );
};

// =============================================================================
// PANEL
// =============================================================================

const Panel: React.FC<{
  title: string;
  right?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
}> = ({ title, right, children, className }) => (
  <div className={`bg-surface border border-grey/10 ${className || ''}`}>
    <div className="flex items-center justify-between gap-4 px-4 py-3 border-b border-grey/10">
      <div className="text-[10px] text-fg/90 tracking-widest">{title}</div>
      {right && <div className="text-[10px] font-mono text-fg/80">{right}</div>}
    </div>
    <div className="p-4">{children}</div>
  </div>
);

// =============================================================================
// EQUITY CURVE CHART
// =============================================================================

const EquityCurveChart: React.FC<{ points: AggregatedRunData['equity']['points'] }> = ({ points }) => {
  const chartData = useMemo(() => {
    if (points.length < 2) return null;
    const equities = points.map((p) => p.equity_value);
    const minV = Math.min(...equities);
    const maxV = Math.max(...equities);
    const span = Math.max(1e-9, maxV - minV);

    const w = 1000;
    const h = 200;
    const pad = 10;

    const d: string[] = [];
    for (let i = 0; i < points.length; i++) {
      const x = (i / (points.length - 1)) * w;
      const y = (1 - (equities[i] - minV) / span) * (h - 2 * pad) + pad;
      d.push(`${i === 0 ? 'M' : 'L'} ${x.toFixed(2)} ${y.toFixed(2)}`);
    }
    return { d: d.join(' '), w, h, minV, maxV };
  }, [points]);

  if (!chartData) {
    return (
      <div className="h-48 flex items-center justify-center text-[11px] font-mono text-fg/60">
        INSUFFICIENT DATA
      </div>
    );
  }

  return (
    <div>
      <svg viewBox={`0 0 ${chartData.w} ${chartData.h}`} className="w-full h-48">
        <defs>
          <linearGradient id="eqGrad" x1="0%" y1="0%" x2="0%" y2="100%">
            <stop offset="0%" stopColor="#3B82F6" stopOpacity="0.25" />
            <stop offset="100%" stopColor="#3B82F6" stopOpacity="0" />
          </linearGradient>
        </defs>
        <path d={`${chartData.d} L ${chartData.w} ${chartData.h} L 0 ${chartData.h} Z`} fill="url(#eqGrad)" />
        <path d={chartData.d} fill="none" stroke="#3B82F6" strokeWidth="2" />
      </svg>
      <div className="flex justify-between text-[10px] font-mono text-fg/60 mt-2">
        <span>MIN: {formatUsd(chartData.minV)}</span>
        <span>MAX: {formatUsd(chartData.maxV)}</span>
      </div>
    </div>
  );
};

// =============================================================================
// DRAWDOWN CHART
// =============================================================================

const DrawdownChart: React.FC<{ points: AggregatedRunData['drawdown']['points'] }> = ({ points }) => {
  const chartData = useMemo(() => {
    if (points.length < 2) return null;
    const dds = points.map((p) => p.drawdown_bps / 100);
    const maxDd = Math.max(...dds.map(Math.abs));
    const span = Math.max(0.01, maxDd);

    const w = 1000;
    const h = 100;
    const pad = 5;

    const d: string[] = [];
    for (let i = 0; i < points.length; i++) {
      const x = (i / (points.length - 1)) * w;
      const y = (dds[i] / span) * (h - 2 * pad) + pad;
      d.push(`${i === 0 ? 'M' : 'L'} ${x.toFixed(2)} ${y.toFixed(2)}`);
    }
    return { d: d.join(' '), w, h, maxDd };
  }, [points]);

  if (!chartData) {
    return (
      <div className="h-24 flex items-center justify-center text-[11px] font-mono text-fg/60">
        NO DATA
      </div>
    );
  }

  return (
    <div>
      <svg viewBox={`0 0 ${chartData.w} ${chartData.h}`} className="w-full h-24">
        <defs>
          <linearGradient id="ddGrad" x1="0%" y1="0%" x2="0%" y2="100%">
            <stop offset="0%" stopColor="#EF4444" stopOpacity="0.3" />
            <stop offset="100%" stopColor="#EF4444" stopOpacity="0" />
          </linearGradient>
        </defs>
        <path d={`${chartData.d} L ${chartData.w} ${chartData.h} L 0 ${chartData.h} Z`} fill="url(#ddGrad)" />
        <path d={chartData.d} fill="none" stroke="#EF4444" strokeWidth="1.5" />
      </svg>
      <div className="flex justify-between text-[10px] font-mono text-fg/60 mt-1">
        <span>0%</span>
        <span>MAX DD: -{chartData.maxDd.toFixed(2)}%</span>
      </div>
    </div>
  );
};

// =============================================================================
// PNL DISTRIBUTION HISTOGRAM
// =============================================================================

const PnLHistogram: React.FC<{ bins: PnLHistogramBin[] }> = ({ bins }) => {
  if (bins.length < 2) {
    return (
      <div className="h-32 flex items-center justify-center text-[11px] font-mono text-fg/60">
        INSUFFICIENT DATA
      </div>
    );
  }

  const maxCount = Math.max(...bins.map((b) => b.count));

  return (
    <div>
      <div className="flex items-end gap-[2px] h-32">
        {bins.map((b, i) => {
          const heightPct = maxCount > 0 ? (b.count / maxCount) * 100 : 0;
          const isNegative = b.bin_end <= 0;
          const bgCls = isNegative ? 'bg-danger/70' : 'bg-success/70';
          return (
            <div
              key={i}
              className={`flex-1 ${bgCls} transition-all`}
              style={{ height: `${Math.max(2, heightPct)}%` }}
              title={`${formatUsd(b.bin_start)} to ${formatUsd(b.bin_end)}: ${b.count} windows`}
            />
          );
        })}
      </div>
      <div className="flex justify-between text-[10px] font-mono text-fg/50 mt-2">
        <span>LOSSES</span>
        <span>PROFITS</span>
      </div>
    </div>
  );
};

// =============================================================================
// DISCLAIMERS PANEL
// =============================================================================

const DisclaimersPanel: React.FC<{ disclaimers: Disclaimer[] }> = ({ disclaimers }) => {
  if (!disclaimers.length) return null;

  return (
    <div className="bg-danger/5 border-2 border-danger p-4 mb-6">
      <div className="text-[11px] font-mono text-danger tracking-widest mb-3">
        ⚠ UNTRUSTED RESULTS - {disclaimers.length} DISCLAIMER(S)
      </div>
      <div className="space-y-2">
        {disclaimers.slice(0, 5).map((d) => (
          <div key={d.id} className="text-[10px] font-mono text-fg/80">
            <span className="text-danger/80">[{d.severity}]</span> {d.id}: {d.message}
          </div>
        ))}
        {disclaimers.length > 5 && (
          <div className="text-[10px] font-mono text-fg/60">
            ... and {disclaimers.length - 5} more
          </div>
        )}
      </div>
    </div>
  );
};

// =============================================================================
// PROVENANCE ACCORDION
// =============================================================================

const ProvenanceAccordion: React.FC<{
  data: AggregatedRunData;
  onDownloadManifest: () => void;
}> = ({ data, onDownloadManifest }) => {
  const [expanded, setExpanded] = useState(false);
  const { summary } = data;

  return (
    <div className="border border-grey/20">
      <button
        onClick={() => setExpanded(!expanded)}
        className="w-full flex items-center justify-between px-4 py-3 text-[10px] font-mono text-fg/80 tracking-widest hover:bg-grey/5 transition-colors"
      >
        <span>PROVENANCE & METHODOLOGY</span>
        <span>{expanded ? '[-]' : '[+]'}</span>
      </button>
      {expanded && (
        <div className="px-4 py-4 border-t border-grey/20 text-[11px] font-mono space-y-3">
          <div className="grid grid-cols-2 gap-x-6 gap-y-2 text-fg/80">
            <div><span className="text-fg/50">Dataset Version:</span> {summary.dataset_version}</div>
            <div><span className="text-fg/50">Readiness:</span> {summary.dataset_readiness}</div>
            <div><span className="text-fg/50">Settlement Source:</span> {summary.settlement_source}</div>
            <div><span className="text-fg/50">Settlement Rule:</span> {summary.settlement_rule}</div>
            <div><span className="text-fg/50">Operating Mode:</span> {summary.operating_mode}</div>
            <div><span className="text-fg/50">Production Grade:</span> {summary.operating_mode === 'ProductionGrade' ? 'Yes' : 'No'}</div>
            <div><span className="text-fg/50">Data Range:</span> {formatDateRange(summary.data_range_start_ns, summary.data_range_end_ns)}</div>
            <div><span className="text-fg/50">Created At:</span> {new Date(summary.created_at * 1000).toISOString().slice(0, 19)} UTC</div>
          </div>
          <div className="pt-3 border-t border-grey/10">
            <div className="text-fg/50 mb-2">RUN ID (full):</div>
            <div className="text-fg font-mono text-[10px] break-all bg-grey/5 p-2 border border-grey/10">
              {summary.run_id}
            </div>
          </div>
          <div>
            <div className="text-fg/50 mb-2">MANIFEST HASH (full):</div>
            <div className="text-fg font-mono text-[10px] break-all bg-grey/5 p-2 border border-grey/10">
              {summary.manifest_hash}
            </div>
          </div>
          <div className="pt-3">
            <button
              onClick={onDownloadManifest}
              className="px-4 py-2 border border-grey/30 text-fg/80 hover:bg-grey/10 transition-colors tracking-widest"
            >
              [DOWNLOAD MANIFEST (JSON)]
            </button>
          </div>
        </div>
      )}
    </div>
  );
};

// =============================================================================
// COPY BUTTON
// =============================================================================

const CopyButton: React.FC<{ text: string; label: string }> = ({ text, label }) => {
  const [copied, setCopied] = useState(false);

  const handleCopy = async () => {
    const success = await copyToClipboard(text);
    if (success) {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  };

  return (
    <button
      onClick={handleCopy}
      className="px-3 py-1 text-[10px] font-mono border border-grey/30 text-fg/80 hover:bg-grey/10 transition-colors tracking-widest"
    >
      {copied ? '[COPIED]' : `[${label}]`}
    </button>
  );
};

// =============================================================================
// INTEGRITY ERROR VIEW
// =============================================================================

const IntegrityErrorView: React.FC<{ error: ManifestIntegrityError }> = ({ error }) => (
  <div className="p-6 h-full">
    <div className="bg-danger/10 border-2 border-danger p-6">
      <div className="text-danger text-2xl mb-2" aria-hidden="true">✗</div>
      <h2 className="text-danger font-mono text-lg tracking-widest mb-4">ARTIFACT INTEGRITY ERROR</h2>
      <p className="text-fg/80 font-mono text-sm mb-4">{error.message}</p>
      <div className="text-[11px] font-mono text-fg/60 space-y-1">
        <div>Run ID: {error.runId}</div>
        <div>Expected Hash: {error.expectedHash}</div>
        <div className="mt-2">Actual Hashes:</div>
        {Object.entries(error.actualHashes).map(([endpoint, hash]) => (
          <div key={endpoint} className="ml-4">
            {endpoint}: {hash} {hash !== error.expectedHash && <span className="text-danger">(MISMATCH)</span>}
          </div>
        ))}
      </div>
      <p className="text-danger/80 font-mono text-xs mt-4">
        Charts and data are NOT rendered due to integrity failure.
      </p>
    </div>
  </div>
);

// =============================================================================
// LOADING STATE
// =============================================================================

const LoadingView: React.FC = () => (
  <div className="p-6 h-full flex items-center justify-center">
    <div className="text-fg/80 font-mono text-sm tracking-widest animate-pulse">
      LOADING RUN DATA...
    </div>
  </div>
);

// =============================================================================
// ERROR STATE
// =============================================================================

const ErrorView: React.FC<{ message: string; onRetry: () => void }> = ({ message, onRetry }) => (
  <div className="p-6 h-full">
    <div className="bg-danger/10 border-2 border-danger p-6 text-center">
      <div className="text-danger text-lg mb-2" aria-hidden="true">✗</div>
      <h3 className="text-danger font-mono text-sm tracking-widest mb-2">FAILED TO LOAD RUN</h3>
      <p className="text-fg/80 font-mono text-xs">{message}</p>
      <button
        onClick={onRetry}
        className="mt-4 bg-danger/20 hover:bg-danger/30 border border-danger text-danger font-mono text-xs tracking-widest py-2 px-4"
      >
        [RETRY]
      </button>
    </div>
  </div>
);

// =============================================================================
// INVALID RUN ID
// =============================================================================

const InvalidRunIdView: React.FC = () => (
  <div className="p-6 h-full">
    <div className="bg-warning/10 border-2 border-warning p-6 text-center">
      <div className="text-warning text-lg mb-2" aria-hidden="true">?</div>
      <h3 className="text-warning font-mono text-sm tracking-widest mb-2">INVALID RUN ID</h3>
      <p className="text-fg/80 font-mono text-xs">The run ID in the URL is empty or invalid.</p>
    </div>
  </div>
);

// =============================================================================
// MAIN RUN PAGE COMPONENT
// =============================================================================

export interface RunPageProps {
  runId: string;
}

export const RunPage: React.FC<RunPageProps> = ({ runId }) => {
  const [data, setData] = useState<AggregatedRunData | null>(null);
  const [integrityError, setIntegrityError] = useState<ManifestIntegrityError | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Validate run_id
  const isValidRunId = useMemo(() => runId && runId.trim().length > 0, [runId]);

  const loadData = useCallback(async () => {
    if (!isValidRunId) return;

    setLoading(true);
    setError(null);
    setIntegrityError(null);

    try {
      const result = await api.getAggregatedRunData(runId);
      
      if ('type' in result && result.type === 'manifest_mismatch') {
        setIntegrityError(result as ManifestIntegrityError);
        setData(null);
      } else {
        setData(result as AggregatedRunData);
        setIntegrityError(null);
      }
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : 'Failed to load run data';
      setError(msg);
    } finally {
      setLoading(false);
    }
  }, [runId, isValidRunId]);

  useEffect(() => {
    loadData();
  }, [loadData]);

  const handleDownloadManifest = useCallback(() => {
    const url = api.getManifestUrl(runId);
    const a = document.createElement('a');
    a.href = url;
    a.download = `manifest_${runId}.json`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
  }, [runId]);

  const currentUrl = useMemo(() => window.location.href, []);

  // Invalid run ID
  if (!isValidRunId) {
    return <InvalidRunIdView />;
  }

  // Loading
  if (loading) {
    return <LoadingView />;
  }

  // Integrity error
  if (integrityError) {
    return <IntegrityErrorView error={integrityError} />;
  }

  // Network/fetch error
  if (error || !data) {
    return <ErrorView message={error || 'No data available'} onRetry={loadData} />;
  }

  const { summary, equity, drawdown, distribution } = data;
  const trustLevel = summary.trust_level;
  const isUntrusted = trustLevel !== 'Trusted';
  const disclaimers = summary.disclaimers?.disclaimers || [];
  const manifestHashShort = truncateHash(summary.manifest_hash, 12);

  // Validate required footer fields - render hard error if any are missing
  const footerData: CertifiedRunFooterData = {
    run_id: summary.run_id,
    publish_timestamp: summary.publish_timestamp ?? summary.created_at,
    schema_version: summary.schema_version,
    manifest_hash: summary.manifest_hash,
  };
  const footerValidation = validateFooterData(footerData);

  if (!footerValidation.isValid) {
    return <FooterMissingFieldsError missingFields={footerValidation.missingFields} />;
  }

  return (
    <div className="min-h-screen bg-void text-fg pb-16">
      {/* Fixed Header with Run ID and Manifest Hash */}
      <header className="sticky top-0 z-50 bg-void border-b border-grey/20 px-4 py-3">
        <div className="max-w-6xl mx-auto">
          <div className="flex flex-col md:flex-row md:items-center md:justify-between gap-3">
            {/* Left: Strategy + Trust */}
            <div className="flex items-center gap-4 flex-wrap">
              <h1 className="text-lg font-mono text-fg">
                {summary.strategy_id?.name || 'BACKTEST'}{' '}
                <span className="text-fg/60">v{summary.strategy_id?.version || '?'}</span>
              </h1>
              <TrustBadge level={trustLevel} />
            </div>
            {/* Right: IDs and Actions */}
            <div className="flex items-center gap-3 flex-wrap">
              <div className="text-[10px] font-mono text-fg/60">
                <span className="text-fg/40">RUN:</span> {truncateHash(runId, 8)}
              </div>
              <div className="text-[10px] font-mono text-fg/60">
                <span className="text-fg/40">HASH:</span>{' '}
                <span className="text-fg/90">{manifestHashShort}</span>
              </div>
              <CopyButton text={currentUrl} label="COPY LINK" />
              <CopyButton text={summary.manifest_hash} label="COPY HASH" />
            </div>
          </div>
        </div>
      </header>

      {/* Main Content */}
      <main className="max-w-6xl mx-auto p-4 md:p-6">
        {/* Disclaimers (prominent if untrusted) */}
        {isUntrusted && disclaimers.length > 0 && (
          <DisclaimersPanel disclaimers={disclaimers} />
        )}

        {/* Time Range */}
        <div className="text-[10px] text-fg/60 font-mono tracking-widest mb-4">
          DATA RANGE: {formatDateRange(summary.data_range_start_ns, summary.data_range_end_ns)}
        </div>

        {/* KPI Summary */}
        <div className="grid grid-cols-2 md:grid-cols-3 lg:grid-cols-6 gap-3 mb-6">
          <KpiCard
            label="NET PnL"
            value={formatUsd(summary.summary.net_pnl)}
            tone={summary.summary.net_pnl >= 0 ? 'success' : 'danger'}
          />
          <KpiCard
            label="GROSS PnL"
            value={formatUsd(summary.summary.gross_pnl)}
          />
          <KpiCard
            label="TOTAL FEES"
            value={formatUsd(summary.summary.total_fees)}
          />
          <KpiCard
            label="MAX DRAWDOWN"
            value={formatPct(-summary.summary.max_drawdown_pct)}
            sub={formatUsd(-summary.summary.max_drawdown)}
            tone="danger"
          />
          <KpiCard
            label="WIN RATE"
            value={formatPct(summary.summary.win_rate)}
            sub={`${summary.summary.windows_traded} / ${summary.summary.total_windows}`}
          />
          <KpiCard
            label="WINDOWS TRADED"
            value={String(summary.summary.windows_traded)}
          />
        </div>

        {/* Equity Curve */}
        <Panel title="EQUITY CURVE" right={`${equity.points.length} POINTS`} className="mb-4">
          <EquityCurveChart points={equity.points} />
        </Panel>

        {/* Drawdown */}
        <Panel title="DRAWDOWN" className="mb-4">
          <DrawdownChart points={drawdown.points} />
        </Panel>

        {/* PnL Distribution */}
        <Panel
          title="PnL DISTRIBUTION (15M WINDOWS)"
          right={`${distribution.total_windows} WINDOWS`}
          className="mb-6"
        >
          <PnLHistogram bins={distribution.bins} />
          <div className="text-[10px] font-mono text-fg/50 mt-2">
            {distribution.winning_windows} winning / {distribution.losing_windows} losing
          </div>
        </Panel>

        {/* Provenance Accordion */}
        <ProvenanceAccordion data={data} onDownloadManifest={handleDownloadManifest} />
      </main>

      {/* Mandatory Certified Run Footer - anti-tamper provenance signal */}
      <CertifiedRunFooter
        runId={footerData.run_id!}
        publishTimestamp={footerData.publish_timestamp!}
        schemaVersion={footerData.schema_version!}
        manifestHash={footerData.manifest_hash!}
      />
    </div>
  );
};

export default RunPage;
