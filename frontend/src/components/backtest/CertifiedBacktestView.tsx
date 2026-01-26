/**
 * CertifiedBacktestView
 * 
 * IMPORTANT: This component renders immutable certified backtest artifacts.
 * 
 * Key behaviors:
 * 1. Uses useCertifiedRun hook for production-grade data fetching with ETag caching
 * 2. Never auto-refreshes or background-polls - published runs are immutable
 * 3. Shows certified artifact indicator (manifest hash + publish timestamp)
 * 4. Forces full re-render on artifact mismatch (different ETag or manifest hash)
 * 5. Fail-closed: cannot render metrics without trust status and manifest
 * 
 * This is intentional for institutional and audit-facing use.
 */

import React, { useCallback, useMemo } from 'react';
import type { TrustLevel, Disclaimer, CertifiedBacktestResults } from '../../types/backtest';
import type { ArtifactMeta, MismatchInfo } from '../../services/certifiedRunCache';
import { TrustGatedContent } from './TrustSignaling';
import { ProvenancePanel } from './ProvenancePanel';
import { ExportControls } from './ExportControls';
import { useCertifiedRun } from '../../hooks/useCertifiedRun';
import { CertifiedArtifactFooter, CacheStatusBadge } from './CertifiedArtifactIndicator';

function formatUsd(v: number | null | undefined, digits: number = 2): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '---';
  const sign = v < 0 ? '-' : '';
  return `${sign}$${Math.abs(v).toLocaleString('en-US', { minimumFractionDigits: digits, maximumFractionDigits: digits })}`;
}

function formatPct(v: number | null | undefined, digits: number = 1): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '---';
  return `${(v * 100).toFixed(digits)}%`;
}

function formatNum(v: number | null | undefined, digits: number = 2): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '---';
  return v.toFixed(digits);
}

const Panel: React.FC<{
  title: string;
  right?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
}> = ({ title, right, children, className }) => {
  return (
    <div className={`bg-surface border border-grey/10 ${className || ''}`}>
      <div className="flex items-center justify-between gap-4 px-4 py-3 border-b border-grey/10">
        <div className="text-[10px] text-fg/90 tracking-widest">{title}</div>
        {right ? <div className="text-[10px] font-mono text-fg/80">{right}</div> : null}
      </div>
      <div className="p-4">{children}</div>
    </div>
  );
};

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
      {sub ? <div className="text-[10px] font-mono text-fg/80 mt-1">{sub}</div> : null}
    </div>
  );
};

interface CertifiedBacktestViewProps {
  runId: string;
}

export const CertifiedBacktestView: React.FC<CertifiedBacktestViewProps> = ({ runId }) => {
  // Use the production-grade data fetching hook with ETag caching
  const {
    data: results,
    meta,
    loading,
    error,
    fromCache,
    notModified,
    mismatch,
    refresh,
  } = useCertifiedRun(runId, {
    onMismatch: (info) => {
      console.warn(
        `[CertifiedBacktestView] Artifact mismatch detected: ${info.type}`,
        `previous=${info.previous}, current=${info.current}`
      );
    },
  });

  const equityCurve = useMemo(() => results?.equity_curve ?? [], [results?.equity_curve]);

  const equityPath = useMemo(() => {
    if (equityCurve.length < 2) return null;

    const pts = equityCurve.map((p) => p.equity_value);
    const minV = Math.min(...pts);
    const maxV = Math.max(...pts);
    const span = Math.max(1e-9, maxV - minV);

    const w = 1000;
    const h = 200;
    const padding = 10;

    const d: string[] = [];
    for (let i = 0; i < pts.length; i++) {
      const x = (i / (pts.length - 1)) * w;
      const y = (1 - (pts[i] - minV) / span) * (h - 2 * padding) + padding;
      d.push(`${i === 0 ? 'M' : 'L'} ${x.toFixed(2)} ${y.toFixed(2)}`);
    }

    return { d: d.join(' '), w, h, minV, maxV };
  }, [equityCurve]);

  // Handle manual refresh
  const handleRefresh = useCallback(() => {
    refresh();
  }, [refresh]);

  if (loading) {
    return (
      <div className="p-6 h-full flex items-center justify-center">
        <div className="text-fg/80 font-mono text-sm tracking-widest">LOADING CERTIFIED RESULTS...</div>
      </div>
    );
  }

  // When there's an error or no data, show the empty dashboard structure
  const hasData = !error && results !== null;
  const trustLevel: TrustLevel | undefined = results?.trust_level;
  const disclaimers: Disclaimer[] | undefined = results?.disclaimers?.disclaimers;
  const manifestUrl: string | undefined = results?.manifest_url;

  const summary = results?.summary;
  const pnlTone = summary?.net_pnl ? (summary.net_pnl > 0 ? 'success' : 'danger') : 'default';
  const pfTone = summary?.profit_factor ? (summary.profit_factor >= 1.0 ? 'success' : 'danger') : 'default';

  // Render the dashboard UI (works with or without data)
  return (
    <div className="p-6 h-full overflow-y-auto">
      {/* When we have valid trust data, wrap in TrustGatedContent */}
      {hasData && trustLevel && manifestUrl ? (
        <TrustGatedContent
          trustLevel={trustLevel}
          disclaimers={disclaimers}
          manifestUrl={manifestUrl}
          runId={results?.run_id}
        >
          <DashboardContent
            hasData={hasData}
            results={results}
            summary={summary}
            equityCurve={equityCurve}
            equityPath={equityPath}
            pnlTone={pnlTone}
            pfTone={pfTone}
            trustLevel={trustLevel}
            disclaimers={disclaimers}
            manifestUrl={manifestUrl}
            fromCache={fromCache}
            notModified={notModified}
            mismatch={mismatch}
            meta={meta}
            runId={runId}
            error={error}
            onRefresh={handleRefresh}
          />
        </TrustGatedContent>
      ) : (
        <DashboardContent
          hasData={hasData}
          results={results}
          summary={summary}
          equityCurve={equityCurve}
          equityPath={equityPath}
          pnlTone={pnlTone}
          pfTone={pfTone}
          trustLevel={trustLevel}
          disclaimers={disclaimers}
          manifestUrl={manifestUrl}
          fromCache={fromCache}
          notModified={notModified}
          mismatch={mismatch}
          meta={meta}
          runId={runId}
          error={error}
          onRefresh={handleRefresh}
        />
      )}
    </div>
  );
};

// Extracted dashboard content component for reuse
const DashboardContent: React.FC<{
  hasData: boolean;
  results: CertifiedBacktestResults | null;
  summary: CertifiedBacktestResults['summary'] | undefined;
  equityCurve: { equity_value: number }[];
  equityPath: { d: string; w: number; h: number; minV: number; maxV: number } | null;
  pnlTone: 'default' | 'success' | 'danger';
  pfTone: 'default' | 'success' | 'danger';
  trustLevel: TrustLevel | undefined;
  disclaimers: Disclaimer[] | undefined;
  manifestUrl: string | undefined;
  fromCache: boolean;
  notModified: boolean;
  mismatch: MismatchInfo | null;
  meta: ArtifactMeta | null;
  runId: string;
  error: string | null;
  onRefresh: () => void;
}> = ({
  hasData,
  results,
  summary,
  equityCurve,
  equityPath,
  pnlTone,
  pfTone,
  trustLevel,
  disclaimers,
  manifestUrl,
  fromCache,
  notModified,
  mismatch,
  meta,
  runId,
  error,
  onRefresh,
}) => (
  <>
    <div className="flex items-end justify-between mb-5 border-b border-grey/20 pb-4">
      <div>
        <div className="flex items-center gap-3 flex-wrap">
          <h1 className="text-2xl font-semibold text-fg font-mono">CERTIFIED BACKTEST RESULTS</h1>
          {hasData && <CacheStatusBadge fromCache={fromCache} notModified={notModified} />}
          {hasData && mismatch && (
            <span className="px-2 py-0.5 text-[9px] font-mono tracking-widest border border-warning/30 text-warning/70 bg-warning/5">
              UPDATED
            </span>
          )}
          {!hasData && (
            <span className="px-2 py-0.5 text-[9px] font-mono tracking-widest border border-grey/30 text-fg/50 bg-grey/5">
              AWAITING DATA
            </span>
          )}
        </div>
        <div className="text-[10px] text-fg/90 tracking-widest mt-1">
          RUN ID: {results?.run_id ?? '---'}
        </div>
      </div>
      <div className="flex items-center gap-4">
        {hasData && results?.provenance?.dataset ? (
          <div className="text-right">
            <div className="text-[10px] text-fg/90 tracking-widest mb-1">DATASET</div>
            <div className="text-sm font-mono text-fg">
              {results.provenance.dataset.classification} / {results.provenance.dataset.readiness}
            </div>
          </div>
        ) : (
          <div className="text-right">
            <div className="text-[10px] text-fg/90 tracking-widest mb-1">DATASET</div>
            <div className="text-sm font-mono text-fg/50">---</div>
          </div>
        )}
        {!hasData && (
          <button
            onClick={onRefresh}
            className="px-3 py-1.5 border border-grey/30 text-fg/80 hover:bg-grey/10 transition-colors font-mono text-[10px] tracking-widest"
          >
            [REFRESH]
          </button>
        )}
      </div>
    </div>

    {/* Error banner (subtle, not blocking) */}
    {error && (
      <div className="mb-4 p-3 bg-warning/5 border border-warning/20 text-[11px] font-mono text-warning/80">
        {error}
      </div>
    )}

    <div className="grid grid-cols-2 md:grid-cols-6 gap-4 mb-4">
      <KpiCard
        label="NET PnL"
        value={formatUsd(summary?.net_pnl)}
        tone={pnlTone}
      />
      <KpiCard
        label="MAX DRAWDOWN"
        value={formatUsd(summary?.max_drawdown)}
        sub={summary?.max_drawdown_pct != null ? `${(summary.max_drawdown_pct * 100).toFixed(1)}%` : undefined}
        tone="danger"
      />
      <KpiCard
        label="WIN RATE"
        value={formatPct(summary?.win_rate)}
        sub={`${summary?.windows_traded ?? 0} / ${summary?.total_windows ?? 0} windows`}
      />
      <KpiCard
        label="SHARPE RATIO"
        value={formatNum(summary?.sharpe_ratio)}
      />
      <KpiCard
        label="PROFIT FACTOR"
        value={formatNum(summary?.profit_factor)}
        tone={pfTone}
      />
      <KpiCard
        label="TOTAL FEES"
        value={formatUsd(summary?.total_fees)}
      />
    </div>

    <Panel
      title="EQUITY CURVE"
      right={equityCurve.length > 0 ? `${equityCurve.length} POINTS` : 'NO DATA'}
      className="mb-4"
    >
      {equityPath ? (
        <div>
          <svg viewBox={`0 0 ${equityPath.w} ${equityPath.h}`} className="w-full h-48">
            <defs>
              <linearGradient id="equityGradient" x1="0%" y1="0%" x2="0%" y2="100%">
                <stop offset="0%" stopColor="#3B82F6" stopOpacity="0.3" />
                <stop offset="100%" stopColor="#3B82F6" stopOpacity="0" />
              </linearGradient>
            </defs>
            <path
              d={`${equityPath.d} L ${equityPath.w} ${equityPath.h} L 0 ${equityPath.h} Z`}
              fill="url(#equityGradient)"
            />
            <path d={equityPath.d} fill="none" stroke="#3B82F6" strokeWidth="2" />
          </svg>
          <div className="flex justify-between text-[10px] font-mono text-fg/80 mt-2">
            <span>MIN: {formatUsd(equityPath.minV)}</span>
            <span>MAX: {formatUsd(equityPath.maxV)}</span>
          </div>
        </div>
      ) : (
        <div className="h-48 flex items-center justify-center text-[11px] font-mono text-fg/50">
          {hasData ? 'NO EQUITY DATA AVAILABLE' : 'EQUITY CURVE WILL APPEAR HERE'}
        </div>
      )}
    </Panel>

    {/* Provenance Panel - show placeholder when no data */}
    {hasData && results?.provenance ? (
      <ProvenancePanel
        runId={results.run_id}
        provenance={results.provenance}
        trustLevel={trustLevel ?? 'Unknown'}
        trustReason={
          trustLevel === 'Untrusted' && disclaimers?.length
            ? disclaimers[0].message
            : undefined
        }
        manifestUrl={manifestUrl}
        productionGrade={results.provenance.operating_mode === 'ProductionGrade'}
        className="mb-4"
      />
    ) : (
      <Panel title="PROVENANCE & METHODOLOGY" className="mb-4">
        <div className="text-[11px] font-mono text-fg/50 text-center py-4">
          Provenance data will appear here once a certified run is loaded.
        </div>
      </Panel>
    )}

    {/* Export controls - only show when we have data */}
    {hasData && (
      <ExportControls
        runId={runId}
        results={results as CertifiedBacktestResults}
        trustLevel={trustLevel}
        manifestHash={meta?.manifestHash}
        className="mb-4"
      />
    )}

    {/* Certified artifact footer */}
    {hasData ? (
      <CertifiedArtifactFooter meta={meta} className="mt-6 -mx-6 -mb-6" />
    ) : (
      <div className="mt-6 -mx-6 -mb-6 px-6 py-4 bg-surface border-t border-grey/10">
        <div className="text-[10px] font-mono text-fg/40 text-center tracking-widest">
          ARTIFACT PROVENANCE FOOTER WILL APPEAR ONCE A RUN IS LOADED
        </div>
      </div>
    )}
  </>
);
