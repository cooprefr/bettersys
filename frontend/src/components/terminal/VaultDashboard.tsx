import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { api } from '../../services/api';
import type {
  VaultActivityRecord,
  VaultActivityResponse,
  VaultConfigResponse,
  VaultLlmDecisionRow,
  VaultLlmModelRecordRow,
  VaultOverviewResponse,
  VaultPerformanceResponse,
  VaultPositionResponse,
  VaultPositionsResponse,
  VaultStateResponse,
} from '../../types/vault';

type VaultTab = 'OVERVIEW' | 'PERFORMANCE' | 'PORTFOLIO' | 'RISK' | 'ACTIVITY' | 'TRANSPARENCY';

function formatUsd(v: number | null | undefined, digits: number = 2): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '---';
  return `$${v.toLocaleString('en-US', { minimumFractionDigits: digits, maximumFractionDigits: digits })}`;
}

function formatPx(v: number | null | undefined, digits: number = 4): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '---';
  return v.toFixed(digits);
}

function formatMaybe(v: number | null | undefined, digits: number = 2): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '---';
  return v.toFixed(digits);
}

function formatTs(tsSec: number | null | undefined): string {
  if (typeof tsSec !== 'number' || !Number.isFinite(tsSec)) return '---';
  try {
    return new Date(tsSec * 1000).toISOString().slice(0, 19).replace('T', ' ');
  } catch {
    return '---';
  }
}

function shortHex(addr: string | null | undefined): string {
  const s = String(addr || '').trim();
  if (!s) return '---';
  if (s.length <= 12) return s;
  return `${s.slice(0, 6)}…${s.slice(-4)}`;
}

function formatTteSec(v: number | null | undefined): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '---';
  if (v <= 0) return 'EXPIRED';
  if (v >= 86400) return `${(v / 86400).toFixed(1)}d`;
  if (v >= 3600) return `${(v / 3600).toFixed(1)}h`;
  if (v >= 60) return `${(v / 60).toFixed(1)}m`;
  return `${Math.floor(v)}s`;
}

function formatPct(v: number | null | undefined, digits: number = 1): string {
  if (typeof v !== 'number' || !Number.isFinite(v)) return '---';
  return `${(v * 100).toFixed(digits)}%`;
}

function formatTimeHhMmSs(d: Date | null): string {
  if (!d) return '---';
  return d.toLocaleTimeString('en-US', { hour12: false });
}

function tabButtonClass(active: boolean): string {
  return [
    'px-3 py-2 text-[11px] font-mono border transition-colors duration-150',
    active
      ? 'bg-white text-black border-white font-semibold'
      : 'border-grey/30 text-grey/80 hover:text-white hover:border-grey/50 hover:bg-grey/10',
  ].join(' ');
}

const Badge: React.FC<{
  tone?: 'paper' | 'base' | 'info' | 'success' | 'danger';
  children: React.ReactNode;
}> = ({
  tone = 'info',
  children,
}) => {
  const cls =
    tone === 'paper'
      ? 'border-warning/60 text-warning'
      : tone === 'base'
        ? 'border-better-blue/40 text-better-blue-lavender'
        : tone === 'success'
          ? 'border-success/40 text-success'
          : tone === 'danger'
            ? 'border-danger/40 text-danger'
        : 'border-grey/30 text-grey/80';
  return (
    <span
      className={`px-2 py-0.5 text-[10px] font-mono border ${cls} tracking-widest select-none`}
    >
      {children}
    </span>
  );
};

const Panel: React.FC<{
  title: string;
  right?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
}> = ({ title, right, children, className }) => {
  return (
    <div className={`bg-surface border border-grey/10 ${className || ''}`}>
      <div className="flex items-center justify-between gap-4 px-4 py-3 border-b border-grey/10">
        <div className="text-[10px] text-grey/80 tracking-widest">{title}</div>
        {right ? <div className="text-[10px] font-mono text-grey/70">{right}</div> : null}
      </div>
      <div className="p-4">{children}</div>
    </div>
  );
};

const KpiCard: React.FC<{ label: string; value: string; sub?: string }> = ({ label, value, sub }) => {
  return (
    <div className="bg-surface border border-grey/10 p-4">
      <div className="text-[10px] text-grey/80 tracking-widest mb-2">{label}</div>
      <div className="text-xl font-mono text-white">{value}</div>
      {sub ? <div className="text-[10px] font-mono text-grey/70 mt-1">{sub}</div> : null}
    </div>
  );
};

const Watermark: React.FC<{ text: string }> = ({ text }) => {
  return (
    <div
      className="absolute inset-0 pointer-events-none flex items-center justify-center"
      aria-hidden="true"
    >
      <div className="text-[72px] font-mono text-white/5 tracking-[0.25em] select-none">
        {text}
      </div>
    </div>
  );
};

export const VaultDashboard: React.FC = () => {
  const [vaultState, setVaultState] = useState<VaultStateResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [lastRefreshAt, setLastRefreshAt] = useState<Date | null>(null);

  const [tab, setTab] = useState<VaultTab>('OVERVIEW');

  const [walletAddress, setWalletAddress] = useState('');
  const [depositAmount, setDepositAmount] = useState('100');
  const [withdrawShares, setWithdrawShares] = useState('');
  const [actionStatus, setActionStatus] = useState<string | null>(null);
  const [actionLoading, setActionLoading] = useState(false);

  const [walletHint, setWalletHint] = useState<{ metamask: boolean; chainId?: string }>(
    () => ({ metamask: false })
  );
  const [walletConnectError, setWalletConnectError] = useState<string | null>(null);

  const [fundOverview, setFundOverview] = useState<VaultOverviewResponse | null>(null);

  const [vaultOverview, setVaultOverview] = useState<VaultOverviewResponse | null>(null);
  const [overviewLoading, setOverviewLoading] = useState(false);
  const [overviewError, setOverviewError] = useState<string | null>(null);

  const [perfRange, setPerfRange] = useState<'24h' | '7d' | '30d' | '90d' | 'all'>('7d');
  const [perfData, setPerfData] = useState<VaultPerformanceResponse | null>(null);
  const [perfLoading, setPerfLoading] = useState(false);
  const [perfError, setPerfError] = useState<string | null>(null);

  const [positionsData, setPositionsData] = useState<VaultPositionsResponse | null>(null);
  const [positionsLoading, setPositionsLoading] = useState(false);
  const [positionsError, setPositionsError] = useState<string | null>(null);

  const [activityData, setActivityData] = useState<VaultActivityResponse | null>(null);
  const [activityLoading, setActivityLoading] = useState(false);
  const [activityError, setActivityError] = useState<string | null>(null);
  const [activityWalletOnly, setActivityWalletOnly] = useState(false);

  const [configData, setConfigData] = useState<VaultConfigResponse | null>(null);
  const [configLoading, setConfigLoading] = useState(false);
  const [configError, setConfigError] = useState<string | null>(null);

  const [llmMarketInput, setLlmMarketInput] = useState<string>('');
  const [llmMarketFilter, setLlmMarketFilter] = useState<string>('');
  const [llmDecisions, setLlmDecisions] = useState<VaultLlmDecisionRow[]>([]);
  const [llmLoading, setLlmLoading] = useState(false);
  const [llmError, setLlmError] = useState<string | null>(null);
  const [llmExpandedDecisionId, setLlmExpandedDecisionId] = useState<string | null>(null);
  const [llmModelsByDecision, setLlmModelsByDecision] = useState<Record<string, VaultLlmModelRecordRow[]>>({});
  const [llmModelsLoading, setLlmModelsLoading] = useState<Record<string, boolean>>({});

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const o = await api.getVaultOverview();
      setFundOverview(o);
      setVaultState({
        cash_usdc: o.cash_usdc,
        nav_usdc: o.nav_usdc,
        total_shares: o.total_shares,
        nav_per_share: o.nav_per_share,
      });
      setLastRefreshAt(new Date());
    } catch (e: any) {
      setError(e?.message ?? 'Failed to load vault state');
    } finally {
      setLoading(false);
    }
  }, []);

  const refreshOverview = useCallback(async () => {
    const w = walletAddress.trim();
    const walletOk = /^0x[a-fA-F0-9]{40}$/.test(w);
    if (!walletOk) {
      setVaultOverview(null);
      return;
    }

    setOverviewLoading(true);
    setOverviewError(null);
    try {
      const o = await api.getVaultOverview(w);
      setVaultOverview(o);
    } catch (e: any) {
      setOverviewError(e?.message ?? 'Failed to load wallet position');
    } finally {
      setOverviewLoading(false);
    }
  }, [walletAddress]);

  const refreshPerformance = useCallback(async () => {
    setPerfLoading(true);
    setPerfError(null);
    try {
      const p = await api.getVaultPerformance(perfRange);
      setPerfData(p);
    } catch (e: any) {
      setPerfError(e?.message ?? 'Failed to load performance');
    } finally {
      setPerfLoading(false);
    }
  }, [perfRange]);

  const refreshPositions = useCallback(async () => {
    setPositionsLoading(true);
    setPositionsError(null);
    try {
      const resp = await api.getVaultPositions();
      setPositionsData(resp);
    } catch (e: any) {
      setPositionsError(e?.message ?? 'Failed to load positions');
    } finally {
      setPositionsLoading(false);
    }
  }, []);

  const refreshActivity = useCallback(async () => {
    setActivityLoading(true);
    setActivityError(null);
    try {
      const w = walletAddress.trim();
      const walletOk = /^0x[a-fA-F0-9]{40}$/.test(w);
      const wallet = activityWalletOnly && walletOk ? w : undefined;
      const resp = await api.getVaultActivity(200, wallet);
      setActivityData(resp);
    } catch (e: any) {
      setActivityError(e?.message ?? 'Failed to load activity');
    } finally {
      setActivityLoading(false);
    }
  }, [activityWalletOnly, walletAddress]);

  const refreshConfig = useCallback(async () => {
    setConfigLoading(true);
    setConfigError(null);
    try {
      const resp = await api.getVaultConfig();
      setConfigData(resp);
    } catch (e: any) {
      setConfigError(e?.message ?? 'Failed to load config');
    } finally {
      setConfigLoading(false);
    }
  }, []);

  const refreshLlmDecisions = useCallback(async () => {
    setLlmLoading(true);
    setLlmError(null);
    try {
      const slug = llmMarketFilter.trim();
      const resp = await api.getVaultLlmDecisions(200, slug || undefined);
      setLlmDecisions(resp.decisions);
    } catch (e: any) {
      setLlmError(e?.message ?? 'Failed to load LLM decisions');
    } finally {
      setLlmLoading(false);
    }
  }, [llmMarketFilter]);

  const ensureLlmModels = useCallback(async (decisionId: string) => {
    if (!decisionId) return;
    if (Array.isArray(llmModelsByDecision[decisionId])) return;
    if (llmModelsLoading[decisionId]) return;

    setLlmModelsLoading((prev) => ({ ...prev, [decisionId]: true }));
    try {
      const resp = await api.getVaultLlmModels(decisionId, 20);
      setLlmModelsByDecision((prev) => ({ ...prev, [decisionId]: resp.records }));
    } catch {
      setLlmModelsByDecision((prev) => ({ ...prev, [decisionId]: [] }));
    } finally {
      setLlmModelsLoading((prev) => ({ ...prev, [decisionId]: false }));
    }
  }, [llmModelsByDecision, llmModelsLoading]);

  useEffect(() => {
    refresh();
    const t = window.setInterval(refresh, 5_000);
    return () => window.clearInterval(t);
  }, [refresh]);

  useEffect(() => {
    if (tab === 'OVERVIEW') {
      refreshOverview();
      return;
    }
    if (tab === 'PERFORMANCE') {
      refreshPerformance();
      return;
    }
    if (tab === 'PORTFOLIO') {
      refreshPositions();
      return;
    }
    if (tab === 'RISK') {
      refreshConfig();
      return;
    }
    if (tab === 'ACTIVITY') {
      refreshActivity();
      return;
    }
    if (tab === 'TRANSPARENCY') {
      refreshLlmDecisions();
    }
  }, [
    tab,
    refreshActivity,
    refreshConfig,
    refreshLlmDecisions,
    refreshOverview,
    refreshPerformance,
    refreshPositions,
  ]);

  // Non-intrusive wallet hint: detect MetaMask + prefill if already authorized.
  useEffect(() => {
    if (typeof window === 'undefined') return;
    const eth: any = (window as any).ethereum;
    if (!eth || !eth.isMetaMask) return;
    setWalletHint({ metamask: true });

    (async () => {
      try {
        const chainId = (await eth.request({ method: 'eth_chainId' })) as string;
        const accounts = (await eth.request({ method: 'eth_accounts' })) as string[];
        setWalletHint({ metamask: true, chainId });
        if (!walletAddress && accounts?.[0]) {
          setWalletAddress(accounts[0]);
        }
      } catch {
        // ignore
      }
    })();
  }, [walletAddress]);

  const connectMetaMask = useCallback(async () => {
    setWalletConnectError(null);
    const eth: any = (window as any).ethereum;
    if (!eth || !eth.isMetaMask) {
      setWalletConnectError('MetaMask not detected');
      return;
    }

    try {
      const accounts = (await eth.request({ method: 'eth_requestAccounts' })) as string[];
      if (accounts?.[0]) setWalletAddress(accounts[0]);
      const chainId = (await eth.request({ method: 'eth_chainId' })) as string;
      setWalletHint({ metamask: true, chainId });
    } catch (e: any) {
      setWalletConnectError(e?.message || 'Wallet connect failed');
    }
  }, []);

  const sharePrice = vaultState?.nav_per_share || 1.0;
  const sharePriceText = useMemo(() => formatUsd(vaultState?.nav_per_share, 4), [vaultState]);
  const walletTrim = walletAddress.trim();
  const walletLooksEvm = /^0x[a-fA-F0-9]{40}$/.test(walletTrim);
  const nav = vaultState?.nav_usdc;
  const cash = vaultState?.cash_usdc;
  const invested =
    typeof nav === 'number' && typeof cash === 'number' && Number.isFinite(nav) && Number.isFinite(cash)
      ? Math.max(0, nav - cash)
      : null;
  const cashPct =
    typeof nav === 'number' && Number.isFinite(nav) && nav > 0 && typeof cash === 'number' && Number.isFinite(cash)
      ? Math.max(0, Math.min(1, cash / nav))
      : null;

  const depositPreview = useMemo(() => {
    const amt = Number(String(depositAmount || '').trim());
    if (!Number.isFinite(amt) || amt <= 0) return null;
    const minted = amt / Math.max(1e-9, sharePrice);
    return { amt, minted };
  }, [depositAmount, sharePrice]);

  const withdrawPreview = useMemo(() => {
    const sh = Number(String(withdrawShares || '').trim());
    if (!Number.isFinite(sh) || sh <= 0) return null;
    const amt = sh * Math.max(0, sharePrice);
    return { sh, amt };
  }, [withdrawShares, sharePrice]);

  const statusLine = useMemo(() => {
    if (loading) return 'LOADING…';
    if (error) return `ERROR: ${error}`;
    if (!vaultState) return '---';
    return `UPDATED @ ${formatTimeHhMmSs(lastRefreshAt)}`;
  }, [loading, error, vaultState, lastRefreshAt]);

  const paperMode = fundOverview?.paper ?? true;
  const engineEnabled = fundOverview?.engine_enabled ?? false;

  const refreshActiveTab = useCallback(() => {
    if (tab === 'OVERVIEW') {
      refreshOverview();
      return;
    }
    if (tab === 'PERFORMANCE') {
      refreshPerformance();
      return;
    }
    if (tab === 'PORTFOLIO') {
      refreshPositions();
      return;
    }
    if (tab === 'RISK') {
      refreshConfig();
      return;
    }
    if (tab === 'ACTIVITY') {
      refreshActivity();
      return;
    }
    if (tab === 'TRANSPARENCY') {
      refreshLlmDecisions();
    }
  }, [
    tab,
    refreshActivity,
    refreshConfig,
    refreshLlmDecisions,
    refreshOverview,
    refreshPerformance,
    refreshPositions,
  ]);

  const perfPoints = useMemo(() => perfData?.points ?? [], [perfData]);
  const perfPath = useMemo(() => {
    const pts = perfPoints
      .map((p) => ({ t: p.ts, v: p.nav_per_share }))
      .filter((p) => typeof p.v === 'number' && Number.isFinite(p.v));
    if (pts.length < 2) return null;

    const minV = Math.min(...pts.map((p) => p.v));
    const maxV = Math.max(...pts.map((p) => p.v));
    const span = Math.max(1e-9, maxV - minV);

    const w = 1000;
    const h = 220;

    const d: string[] = [];
    for (let i = 0; i < pts.length; i++) {
      const x = (i / (pts.length - 1)) * w;
      const y = (1 - (pts[i].v - minV) / span) * (h - 20) + 10;
      d.push(`${i === 0 ? 'M' : 'L'} ${x.toFixed(2)} ${y.toFixed(2)}`);
    }
    return { d: d.join(' '), w, h };
  }, [perfPoints]);

  const perfStats = useMemo(() => {
    const pts = perfPoints
      .map((p) => p.nav_per_share)
      .filter((v) => typeof v === 'number' && Number.isFinite(v));
    if (pts.length < 2) {
      return {
        rangeReturn: null as number | null,
        maxDrawdown: null as number | null,
      };
    }

    const first = pts[0];
    const last = pts[pts.length - 1];
    const rangeReturn = first > 0 ? last / first - 1 : null;

    let peak = pts[0];
    let maxDd = 0;
    for (const v of pts) {
      if (v > peak) peak = v;
      const dd = peak > 0 ? (peak - v) / peak : 0;
      if (dd > maxDd) maxDd = dd;
    }
    return {
      rangeReturn,
      maxDrawdown: maxDd,
    };
  }, [perfPoints]);

  const positions = useMemo(() => positionsData?.positions ?? [], [positionsData]);
  const activityEvents = useMemo(() => activityData?.events ?? [], [activityData]);
  const activityLedger = useMemo(
    () => activityEvents.filter((e) => e.kind === 'DEPOSIT' || e.kind === 'WITHDRAW'),
    [activityEvents]
  );
  const activityTrades = useMemo(
    () => activityEvents.filter((e) => e.kind === 'TRADE'),
    [activityEvents]
  );

  return (
    <div className="p-6 h-full overflow-y-auto">
      {/* Header */}
      <div className="flex items-end justify-between mb-5 border-b border-grey/20 pb-4">
        <div>
          <div className="flex items-center gap-3 flex-wrap">
            <h1 className="text-3xl font-semibold text-white font-mono">BETTER VAULT</h1>
            <Badge tone={paperMode ? 'paper' : 'success'}>{paperMode ? 'PAPER' : 'LIVE'}</Badge>
            <Badge tone={engineEnabled ? 'success' : 'danger'}>
              {engineEnabled ? 'ENGINE ON' : 'ENGINE OFF'}
            </Badge>
            <Badge tone="base">BASE</Badge>
          </div>
          <div className="text-[10px] text-grey/80 tracking-widest mt-1">
            POOLED QUANT STRATEGY // NAV-PRICED SHARES // EXECUTION VIA CLOB
          </div>
        </div>
        <div className="text-right">
          <div className="text-[10px] text-grey/80 tracking-widest mb-1">NAV / SHARE</div>
          <div className="text-2xl font-semibold text-white font-mono">{sharePriceText}</div>
        </div>
      </div>

      {/* Sub-navigation */}
      <div className="flex flex-wrap gap-2 border border-grey/20 p-2 mb-4 bg-void">
        {(
          [
            ['OVERVIEW', 'OVERVIEW'],
            ['PERFORMANCE', 'PERFORMANCE'],
            ['PORTFOLIO', 'PORTFOLIO'],
            ['RISK', 'RISK'],
            ['ACTIVITY', 'ACTIVITY'],
            ['TRANSPARENCY', 'TRANSPARENCY'],
          ] as Array<[VaultTab, string]>
        ).map(([id, label]) => (
          <button key={id} type="button" onClick={() => setTab(id)} className={tabButtonClass(tab === id)}>
            {label}
          </button>
        ))}
      </div>

      <div className="flex items-center justify-between mb-5">
        <div className="text-[11px] font-mono text-grey/70">{statusLine}</div>
        <button
          type="button"
          onClick={() => {
            refresh();
            refreshActiveTab();
          }}
          className="text-[11px] font-mono border border-grey/20 px-3 py-1 text-grey/80 hover:text-white hover:border-grey/40"
        >
          [REFRESH]
        </button>
      </div>

      {tab === 'OVERVIEW' ? (
        <div className="space-y-6">
          <div className="grid grid-cols-1 md:grid-cols-5 gap-4">
            <KpiCard label="AUM (TVL)" value={formatUsd(vaultState?.nav_usdc, 2)} />
            <KpiCard
              label="CASH (USDC)"
              value={formatUsd(vaultState?.cash_usdc, 2)}
              sub={cashPct != null ? `${formatPct(cashPct, 1)} CASH` : '---'}
            />
            <KpiCard label="INVESTED" value={formatUsd(invested, 2)} sub={cashPct != null ? formatPct(1 - cashPct, 1) + ' INVESTED' : '---'} />
            <KpiCard label="TOTAL SHARES" value={vaultState?.total_shares?.toFixed(6) ?? '---'} />
            <KpiCard
              label="MODE"
              value={paperMode ? 'PAPER' : 'LIVE'}
              sub={engineEnabled ? 'ENGINE ENABLED' : 'ENGINE DISABLED'}
            />
          </div>

          <div className="grid grid-cols-1 lg:grid-cols-3 gap-4">
            <div className="lg:col-span-2 space-y-4">
              <Panel
                title="YOUR POSITION"
                right={walletTrim ? shortHex(walletTrim) : 'NO WALLET'}
              >
                {!walletTrim ? (
                  <div className="text-[11px] font-mono text-grey/70">CONNECT OR PASTE A WALLET ADDRESS TO VIEW SHARES.</div>
                ) : !walletLooksEvm ? (
                  <div className="text-[11px] font-mono text-danger">INVALID WALLET ADDRESS (EXPECTED 0x…40 HEX).</div>
                ) : overviewLoading ? (
                  <div className="text-[11px] font-mono text-grey/70">LOADING…</div>
                ) : overviewError ? (
                  <div className="text-[11px] font-mono text-danger">{overviewError}</div>
                ) : (
                  <div className="grid grid-cols-1 md:grid-cols-3 gap-4 text-[11px] font-mono">
                    <div>
                      <div className="text-grey/70">SHARES</div>
                      <div className="text-white">{formatMaybe(vaultOverview?.user_shares, 6)}</div>
                    </div>
                    <div>
                      <div className="text-grey/70">VALUE (USDC)</div>
                      <div className="text-white">{formatUsd(vaultOverview?.user_value_usdc, 2)}</div>
                    </div>
                    <div>
                      <div className="text-grey/70">NAV / SHARE</div>
                      <div className="text-white">{formatUsd(vaultOverview?.nav_per_share, 4)}</div>
                      <div className="text-[10px] text-grey/70 mt-1">AS OF {formatTs(vaultOverview?.fetched_at)}</div>
                    </div>
                  </div>
                )}
              </Panel>

              <Panel
                title="DEPOSIT (ACCOUNTING ONLY)"
                right={depositPreview ? `PREVIEW ≈ ${depositPreview.minted.toFixed(6)} SHARES` : 'PREVIEW ---'}
                className="relative overflow-hidden"
              >
                <Watermark text={paperMode ? 'PAPER' : 'LIVE'} />
                <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
                  <div className="md:col-span-2 space-y-2">
                    <input
                      value={walletAddress}
                      onChange={(e) => setWalletAddress(e.target.value)}
                      placeholder="wallet address (0x...)"
                      className="w-full bg-black/50 border border-grey/20 px-3 py-2 text-[12px] font-mono text-white"
                    />
                    <div className="flex flex-wrap items-center gap-2">
                      <button
                        type="button"
                        onClick={connectMetaMask}
                        disabled={!walletHint.metamask}
                        className="text-[11px] font-mono border border-grey/20 px-3 py-1 text-grey/80 hover:text-white hover:border-grey/40 disabled:opacity-40"
                        title={walletHint.metamask ? 'Connect MetaMask' : 'MetaMask not detected'}
                      >
                        [CONNECT METAMASK]
                      </button>
                      {walletHint.metamask ? (
                        <span className="text-[10px] font-mono text-grey/70">
                          CHAIN {walletHint.chainId || '---'}
                        </span>
                      ) : (
                        <span className="text-[10px] font-mono text-grey/70">METAMASK NOT DETECTED</span>
                      )}
                      {walletConnectError ? (
                        <span className="text-[10px] font-mono text-danger">{walletConnectError}</span>
                      ) : null}
                    </div>
                  </div>
                  <div className="space-y-2">
                    <input
                      value={depositAmount}
                      onChange={(e) => setDepositAmount(e.target.value)}
                      placeholder="amount_usdc"
                      inputMode="decimal"
                      className="w-full bg-black/50 border border-grey/20 px-3 py-2 text-[12px] font-mono text-white"
                    />
                    <button
                      type="button"
                      disabled={actionLoading || !walletLooksEvm || !depositPreview}
                      onClick={async () => {
                        setActionStatus(null);
                        setActionLoading(true);
                        try {
                          if (!walletLooksEvm || !depositPreview) {
                            setActionStatus('INVALID WALLET OR AMOUNT');
                            return;
                          }
                          const amount = Number(depositAmount);
                          const resp = await api.vaultDeposit({
                            wallet_address: walletTrim,
                            amount_usdc: amount,
                          });
                          setActionStatus(`DEPOSIT OK // MINTED ${resp.shares_minted.toFixed(6)} SHARES @ ${formatUsd(resp.nav_per_share, 4)}`);
                          await refresh();
                          await refreshOverview();
                        } catch (e: any) {
                          setActionStatus(e?.message ?? 'Deposit failed');
                        } finally {
                          setActionLoading(false);
                        }
                      }}
                      className="bg-better-blue hover:bg-blue-700 disabled:opacity-50 text-white font-mono tracking-widest py-3 px-4 transition-colors duration-150 w-full"
                    >
                      [DEPOSIT USDC]
                    </button>
                  </div>
                </div>
              </Panel>

              <Panel
                title="WITHDRAW (ACCOUNTING ONLY)"
                right={withdrawPreview ? `PREVIEW ≈ ${formatUsd(withdrawPreview.amt, 2)}` : 'PREVIEW ---'}
                className="relative overflow-hidden"
              >
                <Watermark text={paperMode ? 'PAPER' : 'LIVE'} />
                <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
                  <div className="md:col-span-2">
                    <input
                      value={walletAddress}
                      onChange={(e) => setWalletAddress(e.target.value)}
                      placeholder="wallet address (0x...)"
                      className="w-full bg-black/50 border border-grey/20 px-3 py-2 text-[12px] font-mono text-white"
                    />
                    <div className="text-[10px] font-mono text-grey/70 mt-2">
                      NOTE: CASH-ONLY WITHDRAWALS (POSITION LIQUIDATION NOT IMPLEMENTED)
                    </div>
                  </div>
                  <div className="space-y-2">
                    <input
                      value={withdrawShares}
                      onChange={(e) => setWithdrawShares(e.target.value)}
                      placeholder="shares"
                      inputMode="decimal"
                      className="w-full bg-black/50 border border-grey/20 px-3 py-2 text-[12px] font-mono text-white"
                    />
                    <button
                      type="button"
                      disabled={actionLoading || !walletLooksEvm || !withdrawPreview}
                      onClick={async () => {
                        setActionStatus(null);
                        setActionLoading(true);
                        try {
                          if (!walletLooksEvm || !withdrawPreview) {
                            setActionStatus('INVALID WALLET OR SHARES');
                            return;
                          }
                          const shares = Number(withdrawShares);
                          const resp = await api.vaultWithdraw({
                            wallet_address: walletTrim,
                            shares,
                          });
                          setActionStatus(`WITHDRAW OK // ${formatUsd(resp.amount_usdc, 2)} @ ${formatUsd(resp.nav_per_share, 4)}`);
                          await refresh();
                          await refreshOverview();
                        } catch (e: any) {
                          setActionStatus(e?.message ?? 'Withdraw failed');
                        } finally {
                          setActionLoading(false);
                        }
                      }}
                      className="bg-surface border border-grey/30 hover:border-better-blue disabled:opacity-50 text-white font-mono tracking-widest py-3 px-4 transition-colors duration-150 w-full"
                    >
                      [WITHDRAW]
                    </button>
                  </div>
                </div>
              </Panel>

              {actionStatus ? <div className="text-[11px] font-mono text-grey/80">{actionStatus}</div> : null}
            </div>

            <div className="space-y-4">
              <Panel title="STRATEGY MODULES" right="DISCLOSURE">
                <div className="space-y-3 text-[11px] font-mono">
                  <div>
                    <div className="text-white">FAST15M (BTC/ETH/SOL/XRP)</div>
                    <div className="text-grey/70">
                      Driftless lognormal p(UP), shrink-to-half conservatism, fractional Kelly, hard caps.
                    </div>
                  </div>
                  <div>
                    <div className="text-white">LONG (BRAID-bounded LLM)</div>
                    <div className="text-grey/70">
                      Scout-first gating + 3-of-4 consensus; deterministic admissibility + cost-adjusted edge.
                    </div>
                  </div>
                </div>
              </Panel>

              <Panel title="TERMS (PLACEHOLDER)" right="PAPER">
                <div className="grid grid-cols-2 gap-3 text-[11px] font-mono">
                  <div>
                    <div className="text-grey/70">MANAGEMENT FEE</div>
                    <div className="text-white">---</div>
                  </div>
                  <div>
                    <div className="text-grey/70">PERFORMANCE FEE</div>
                    <div className="text-white">---</div>
                  </div>
                  <div>
                    <div className="text-grey/70">LIQUIDITY</div>
                    <div className="text-white">T+0 (PAPER)</div>
                  </div>
                  <div>
                    <div className="text-grey/70">CUSTODY</div>
                    <div className="text-white">ACCOUNTING ONLY</div>
                  </div>
                </div>
              </Panel>

              <Panel title="INVESTOR NOTES" right="IMPORTANT">
                <div className="text-[10px] font-mono text-grey/70 space-y-2">
                  <div>PAPER MODE: NO ON-CHAIN DEPOSITS/WITHDRAWALS.</div>
                  <div>LONG ENGINE USES OPENROUTER KEY FROM ENV ONLY (NEVER STORED IN UI).</div>
                  <div>PERFORMANCE/PORTFOLIO/ACTIVITY ARE POWERED BY PERSISTED NAV SNAPSHOTS + ACTIVITY LOG.</div>
                </div>
              </Panel>
            </div>
          </div>

          <div className="text-[10px] text-grey/70 text-center font-mono">
            PAST PERFORMANCE DOES NOT GUARANTEE FUTURE RESULTS. THIS UI IS ACCOUNTING-ONLY UNTIL ON-CHAIN DEPOSITS/WITHDRAWALS ARE WIRED.
          </div>
        </div>
      ) : null}

      {tab === 'PERFORMANCE' ? (
        <div className="space-y-4">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div className="text-[10px] font-mono text-grey/70">RANGE</div>
            <div className="flex flex-wrap gap-2">
              {(['24h', '7d', '30d', '90d', 'all'] as const).map((r) => (
                <button
                  key={r}
                  type="button"
                  onClick={() => setPerfRange(r)}
                  className={
                    'px-2 py-1 text-[10px] font-mono border ' +
                    (perfRange === r
                      ? 'bg-white text-black border-white'
                      : 'border-grey/30 text-grey/80 hover:text-white hover:border-grey/50 hover:bg-grey/10')
                  }
                >
                  {r.toUpperCase()}
                </button>
              ))}
            </div>
          </div>

          <Panel
            title="NAV / SHARE (TIME SERIES)"
            right={
              perfLoading
                ? 'LOADING…'
                : perfError
                  ? 'ERROR'
                  : `RANGE ${perfRange.toUpperCase()} // POINTS ${perfPoints.length}`
            }
            className="relative overflow-hidden"
          >
            <Watermark text={paperMode ? 'PAPER' : 'LIVE'} />
            {perfError ? (
              <div className="text-[11px] font-mono text-danger">{perfError}</div>
            ) : perfLoading ? (
              <div className="h-56 flex items-center justify-center text-[11px] font-mono text-grey/70">LOADING…</div>
            ) : perfPath ? (
              <div>
                <svg viewBox={`0 0 ${perfPath.w} ${perfPath.h}`} className="w-full h-56">
                  <path d={perfPath.d} fill="none" stroke="#3B82F6" strokeWidth="2" />
                </svg>
                <div className="mt-2 text-[10px] font-mono text-grey/70">
                  LAST SNAPSHOT: {formatTs(perfPoints[perfPoints.length - 1]?.ts)}
                </div>
              </div>
            ) : (
              <div className="h-56 flex items-center justify-center text-[11px] font-mono text-grey/70">
                NO NAV SERIES AVAILABLE YET.
              </div>
            )}
          </Panel>

          <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
            <KpiCard label="RANGE RETURN" value={formatPct(perfStats.rangeReturn, 2)} />
            <KpiCard label="MAX DRAWDOWN" value={formatPct(perfStats.maxDrawdown, 2)} />
            <KpiCard label="SNAPSHOTS" value={perfPoints.length ? String(perfPoints.length) : '---'} />
            <KpiCard label="LATEST NAV/SHARE" value={formatUsd(perfPoints[perfPoints.length - 1]?.nav_per_share, 4)} />
          </div>
        </div>
      ) : null}

      {tab === 'PORTFOLIO' ? (
        <div className="space-y-4">
          <Panel
            title="ALLOCATION"
            right={positionsLoading ? 'LOADING…' : `POSITIONS ${positions.length}`}
            className="relative overflow-hidden"
          >
            <Watermark text={paperMode ? 'PAPER' : 'LIVE'} />
            <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
              <KpiCard label="CASH" value={formatUsd(cash, 2)} />
              <KpiCard label="INVESTED" value={formatUsd(invested, 2)} />
              <KpiCard label="CASH RATIO" value={formatPct(cashPct, 1)} />
              <KpiCard label="OPEN POSITIONS" value={String(positions.length)} />
            </div>
          </Panel>

          <Panel
            title="POSITIONS"
            right={positionsLoading ? 'LOADING…' : positionsError ? 'ERROR' : `AS OF ${formatTs(positionsData?.fetched_at)}`}
            className="relative overflow-hidden"
          >
            <Watermark text={paperMode ? 'PAPER' : 'LIVE'} />
            {positionsError ? (
              <div className="text-[11px] font-mono text-danger">{positionsError}</div>
            ) : positionsLoading ? (
              <div className="text-[11px] font-mono text-grey/70">LOADING…</div>
            ) : !positions.length ? (
              <div className="text-[11px] font-mono text-grey/70">NO OPEN POSITIONS.</div>
            ) : (
              <div className="overflow-x-auto">
                <table className="w-full text-[11px] font-mono">
                  <thead>
                    <tr className="text-grey/70 border-b border-grey/20">
                      <th className="text-left py-2 pr-3">MARKET</th>
                      <th className="text-left py-2 pr-3">OUTCOME</th>
                      <th className="text-right py-2 pr-3">SHARES</th>
                      <th className="text-right py-2 pr-3">ENTRY</th>
                      <th className="text-right py-2 pr-3">MID</th>
                      <th className="text-right py-2 pr-3">SPREAD</th>
                      <th className="text-right py-2 pr-3">VALUE</th>
                      <th className="text-right py-2 pr-3">PNL</th>
                      <th className="text-right py-2 pr-3">TTE</th>
                      <th className="text-left py-2 pr-3">STRAT</th>
                      <th className="text-left py-2">DECISION</th>
                    </tr>
                  </thead>
                  <tbody>
                    {positions.map((p: VaultPositionResponse) => {
                      const pnl = p.pnl_unrealized_usdc;
                      const pnlCls =
                        typeof pnl === 'number' && Number.isFinite(pnl)
                          ? pnl >= 0
                            ? 'text-success'
                            : 'text-danger'
                          : 'text-grey/70';
                      const spreadBps =
                        typeof p.spread === 'number' && Number.isFinite(p.spread)
                          ? `${Math.round(p.spread * 10_000)}bps`
                          : '---';

                      return (
                        <tr key={p.token_id} className="border-b border-grey/10">
                          <td className="py-2 pr-3 text-white max-w-[420px] truncate">
                            {p.market_question || p.market_slug || p.token_id}
                          </td>
                          <td className="py-2 pr-3 text-white">{p.outcome}</td>
                          <td className="py-2 pr-3 text-right text-white">{formatMaybe(p.shares, 4)}</td>
                          <td className="py-2 pr-3 text-right text-white">{formatPx(p.avg_price, 4)}</td>
                          <td className="py-2 pr-3 text-right text-white">{formatPx(p.mid, 4)}</td>
                          <td className="py-2 pr-3 text-right text-grey/70">{spreadBps}</td>
                          <td className="py-2 pr-3 text-right text-white">{formatUsd(p.value_usdc, 2)}</td>
                          <td className={`py-2 pr-3 text-right ${pnlCls}`}>{formatUsd(pnl, 2)}</td>
                          <td className="py-2 pr-3 text-right text-grey/70">{formatTteSec(p.tte_sec)}</td>
                          <td className="py-2 pr-3 text-grey/70">{p.strategy || '---'}</td>
                          <td className="py-2 text-grey/70">{p.decision_id ? p.decision_id.slice(0, 10) : '---'}</td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            )}
          </Panel>
        </div>
      ) : null}

      {tab === 'RISK' ? (
        <div className="space-y-4">
          <Panel
            title="GUARDRAILS"
            right={configLoading ? 'LOADING…' : configError ? 'ERROR' : `AS OF ${formatTs(configData?.fetched_at)}`}
            className="relative overflow-hidden"
          >
            <Watermark text={paperMode ? 'PAPER' : 'LIVE'} />

            {configError ? (
              <div className="text-[11px] font-mono text-danger">{configError}</div>
            ) : configLoading ? (
              <div className="text-[11px] font-mono text-grey/70">LOADING…</div>
            ) : !configData ? (
              <div className="text-[11px] font-mono text-grey/70">---</div>
            ) : (
              <div className="grid grid-cols-1 md:grid-cols-2 gap-4 text-[11px] font-mono">
                <div className="space-y-2">
                  <div className="text-white">FAST15M</div>
                  <div className="text-grey/70">POLL = {configData.updown_poll_ms}ms</div>
                  <div className="text-grey/70">MIN_EDGE = {formatPct(configData.updown_min_edge, 2)}</div>
                  <div className="text-grey/70">KELLY_FRACTION = {formatMaybe(configData.updown_kelly_fraction, 4)}</div>
                  <div className="text-grey/70">MAX_POSITION_PCT = {formatPct(configData.updown_max_position_pct, 2)}</div>
                  <div className="text-grey/70">SHRINK_TO_HALF = {formatMaybe(configData.updown_shrink_to_half, 3)}</div>
                  <div className="text-grey/70">COOLDOWN = {configData.updown_cooldown_sec}s</div>
                </div>

                <div className="space-y-2">
                  <div className="text-white">LONG (LLM)</div>
                  <div className="text-grey/70">ENABLED = {configData.long_enabled ? 'TRUE' : 'FALSE'}</div>
                  <div className="text-grey/70">POLL = {configData.long_poll_ms}ms</div>
                  <div className="text-grey/70">MIN_EDGE = {formatPct(configData.long_min_edge, 2)}</div>
                  <div className="text-grey/70">MAX_SPREAD = {formatMaybe(configData.long_max_spread_bps, 0)}bps</div>
                  <div className="text-grey/70">MAX_TTE = {formatMaybe(configData.long_max_tte_days, 1)}d</div>
                  <div className="text-grey/70">MIN_TOP_BOOK = {formatUsd(configData.long_min_top_of_book_usd, 0)}</div>
                  <div className="text-grey/70">BUDGET = {configData.long_max_calls_per_day} calls/day, {configData.long_max_tokens_per_day} tokens/day</div>
                  <div className="text-grey/70">LLM_TIMEOUT = {configData.long_llm_timeout_sec}s</div>
                  <div className="text-grey/70">LLM_MAX_TOKENS = {configData.long_llm_max_tokens}</div>
                  <div className="text-grey/70">LLM_TEMPERATURE = {formatMaybe(configData.long_llm_temperature, 2)}</div>
                  <div className="text-grey/70">MODELS = {configData.long_models?.join(', ') || '---'}</div>
                  {configData.llm_usage_today ? (
                    <div className="text-grey/70">USAGE TODAY = {configData.llm_usage_today.calls_today} calls, {configData.llm_usage_today.tokens_today} tokens</div>
                  ) : null}
                </div>
              </div>
            )}
          </Panel>

          <Panel title="RISK METRICS" right="COMING SOON">
            <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
              <KpiCard label="VAR (95%)" value="---" />
              <KpiCard label="CVAR (95%)" value="---" />
              <KpiCard label="CONCENTRATION" value="---" />
              <KpiCard label="KILL-SWITCH" value="---" />
            </div>
          </Panel>
        </div>
      ) : null}

      {tab === 'ACTIVITY' ? (
        <div className="space-y-4">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div className="text-[10px] font-mono text-grey/70">FILTER</div>
            <div className="flex flex-wrap gap-2">
              <button
                type="button"
                onClick={() => setActivityWalletOnly(false)}
                className={
                  'px-2 py-1 text-[10px] font-mono border ' +
                  (!activityWalletOnly
                    ? 'bg-white text-black border-white'
                    : 'border-grey/30 text-grey/80 hover:text-white hover:border-grey/50 hover:bg-grey/10')
                }
              >
                ALL
              </button>
              <button
                type="button"
                disabled={!walletLooksEvm}
                onClick={() => setActivityWalletOnly(true)}
                className={
                  'px-2 py-1 text-[10px] font-mono border disabled:opacity-40 ' +
                  (activityWalletOnly
                    ? 'bg-white text-black border-white'
                    : 'border-grey/30 text-grey/80 hover:text-white hover:border-grey/50 hover:bg-grey/10')
                }
                title={walletLooksEvm ? 'Show only this wallet' : 'Enter a valid wallet first'}
              >
                THIS WALLET
              </button>
            </div>
          </div>

          <Panel
            title="DEPOSITS / WITHDRAWS"
            right={activityLoading ? 'LOADING…' : activityError ? 'ERROR' : `AS OF ${formatTs(activityData?.fetched_at)}`}
            className="relative overflow-hidden"
          >
            <Watermark text={paperMode ? 'PAPER' : 'LIVE'} />
            {activityError ? (
              <div className="text-[11px] font-mono text-danger">{activityError}</div>
            ) : activityLoading ? (
              <div className="text-[11px] font-mono text-grey/70">LOADING…</div>
            ) : !activityLedger.length ? (
              <div className="text-[11px] font-mono text-grey/70">NO DEPOSITS/WITHDRAWS LOGGED.</div>
            ) : (
              <div className="overflow-x-auto">
                <table className="w-full text-[11px] font-mono">
                  <thead>
                    <tr className="text-grey/70 border-b border-grey/20">
                      <th className="text-left py-2 pr-3">TS</th>
                      <th className="text-left py-2 pr-3">WALLET</th>
                      <th className="text-left py-2 pr-3">KIND</th>
                      <th className="text-right py-2 pr-3">AMOUNT</th>
                      <th className="text-right py-2">SHARES</th>
                    </tr>
                  </thead>
                  <tbody>
                    {activityLedger.map((e: VaultActivityRecord) => (
                      <tr key={e.id} className="border-b border-grey/10">
                        <td className="py-2 pr-3 text-grey/70">{formatTs(e.ts)}</td>
                        <td className="py-2 pr-3 text-white">{shortHex(e.wallet_address)}</td>
                        <td className="py-2 pr-3 text-white">{e.kind}</td>
                        <td className="py-2 pr-3 text-right text-white">{formatUsd(e.amount_usdc, 2)}</td>
                        <td className="py-2 text-right text-white">{formatMaybe(e.shares, 6)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </Panel>

          <Panel
            title="TRADE BLOTTER"
            right={activityLoading ? 'LOADING…' : activityError ? 'ERROR' : `TRADES ${activityTrades.length}`}
            className="relative overflow-hidden"
          >
            <Watermark text={paperMode ? 'PAPER' : 'LIVE'} />
            {activityError ? (
              <div className="text-[11px] font-mono text-danger">{activityError}</div>
            ) : activityLoading ? (
              <div className="text-[11px] font-mono text-grey/70">LOADING…</div>
            ) : !activityTrades.length ? (
              <div className="text-[11px] font-mono text-grey/70">NO TRADES LOGGED.</div>
            ) : (
              <div className="overflow-x-auto">
                <table className="w-full text-[11px] font-mono">
                  <thead>
                    <tr className="text-grey/70 border-b border-grey/20">
                      <th className="text-left py-2 pr-3">TS</th>
                      <th className="text-left py-2 pr-3">STRAT</th>
                      <th className="text-left py-2 pr-3">MARKET</th>
                      <th className="text-left py-2 pr-3">OUTCOME</th>
                      <th className="text-left py-2 pr-3">SIDE</th>
                      <th className="text-right py-2 pr-3">PRICE</th>
                      <th className="text-right py-2 pr-3">NOTIONAL</th>
                      <th className="text-right py-2 pr-3">SHARES</th>
                      <th className="text-left py-2">DECISION</th>
                    </tr>
                  </thead>
                  <tbody>
                    {activityTrades.map((e: VaultActivityRecord) => (
                      <tr key={e.id} className="border-b border-grey/10">
                        <td className="py-2 pr-3 text-grey/70">{formatTs(e.ts)}</td>
                        <td className="py-2 pr-3 text-grey/70">{e.strategy || '---'}</td>
                        <td className="py-2 pr-3 text-white max-w-[420px] truncate">{e.market_slug || e.token_id || '---'}</td>
                        <td className="py-2 pr-3 text-white">{e.outcome || '---'}</td>
                        <td className="py-2 pr-3 text-white">{e.side || '---'}</td>
                        <td className="py-2 pr-3 text-right text-white">{formatPx(e.price, 4)}</td>
                        <td className="py-2 pr-3 text-right text-white">{formatUsd(e.notional_usdc, 2)}</td>
                        <td className="py-2 pr-3 text-right text-white">{formatMaybe(e.shares, 4)}</td>
                        <td className="py-2 text-grey/70">{e.decision_id ? e.decision_id.slice(0, 10) : '---'}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </Panel>
        </div>
      ) : null}

      {tab === 'TRANSPARENCY' ? (
        <div className="space-y-4">
          <Panel
            title="LLM DECISIONS"
            right={llmLoading ? 'LOADING…' : llmError ? 'ERROR' : `DECISIONS ${llmDecisions.length}`}
            className="relative overflow-hidden"
          >
            <Watermark text={paperMode ? 'PAPER' : 'LIVE'} />

            <div className="flex flex-wrap items-center gap-2 mb-3">
              <input
                value={llmMarketInput}
                onChange={(e) => setLlmMarketInput(e.target.value)}
                placeholder="market_slug filter (optional)"
                className="bg-black/50 border border-grey/20 px-3 py-2 text-[12px] font-mono text-white w-full md:w-[520px]"
              />
              <button
                type="button"
                onClick={() => {
                  setLlmExpandedDecisionId(null);
                  setLlmMarketFilter(llmMarketInput.trim());
                }}
                className="text-[11px] font-mono border border-grey/20 px-3 py-2 text-grey/80 hover:text-white hover:border-grey/40"
              >
                [APPLY]
              </button>
              <button
                type="button"
                onClick={() => {
                  setLlmExpandedDecisionId(null);
                  setLlmMarketInput('');
                  setLlmMarketFilter('');
                }}
                className="text-[11px] font-mono border border-grey/20 px-3 py-2 text-grey/80 hover:text-white hover:border-grey/40"
              >
                [CLEAR]
              </button>
              <button
                type="button"
                onClick={refreshLlmDecisions}
                className="text-[11px] font-mono border border-grey/20 px-3 py-2 text-grey/80 hover:text-white hover:border-grey/40"
              >
                [REFRESH]
              </button>
            </div>

            {llmError ? (
              <div className="text-[11px] font-mono text-danger">{llmError}</div>
            ) : llmLoading ? (
              <div className="text-[11px] font-mono text-grey/70">LOADING…</div>
            ) : !llmDecisions.length ? (
              <div className="text-[11px] font-mono text-grey/70">NO DECISIONS LOGGED.</div>
            ) : (
              <div className="overflow-x-auto">
                <table className="w-full text-[11px] font-mono">
                  <thead>
                    <tr className="text-grey/70 border-b border-grey/20">
                      <th className="text-left py-2 pr-3">TS</th>
                      <th className="text-left py-2 pr-3">MARKET</th>
                      <th className="text-left py-2 pr-3">ACTION</th>
                      <th className="text-left py-2 pr-3">OUTCOME</th>
                      <th className="text-right py-2 pr-3">P_TRUE</th>
                      <th className="text-right py-2 pr-3">P_EFF</th>
                      <th className="text-right py-2 pr-3">EDGE</th>
                      <th className="text-right py-2 pr-3">SIZE</th>
                      <th className="text-left py-2 pr-3">MODELS</th>
                      <th className="text-left py-2 pr-3">FLAGS</th>
                      <th className="text-left py-2">DECISION</th>
                    </tr>
                  </thead>
                  <tbody>
                    {llmDecisions.map((d: VaultLlmDecisionRow) => {
                      const expanded = llmExpandedDecisionId === d.decision_id;
                      return (
                        <React.Fragment key={d.decision_id}>
                          <tr
                            className={`border-b border-grey/10 cursor-pointer ${expanded ? 'bg-grey/5' : ''}`}
                            onClick={() => {
                              const next = expanded ? null : d.decision_id;
                              setLlmExpandedDecisionId(next);
                              if (!expanded) ensureLlmModels(d.decision_id);
                            }}
                          >
                            <td className="py-2 pr-3 text-grey/70">{formatTs(d.created_at)}</td>
                            <td className="py-2 pr-3 text-white max-w-[380px] truncate">{d.market_slug}</td>
                            <td className="py-2 pr-3 text-white">{d.action}</td>
                            <td className="py-2 pr-3 text-white">{d.outcome_text || (typeof d.outcome_index === 'number' ? String(d.outcome_index) : '---')}</td>
                            <td className="py-2 pr-3 text-right text-white">{formatMaybe(d.p_true, 3)}</td>
                            <td className="py-2 pr-3 text-right text-white">{formatMaybe(d.p_eff, 3)}</td>
                            <td className="py-2 pr-3 text-right text-white">{formatMaybe(d.edge, 3)}</td>
                            <td className="py-2 pr-3 text-right text-white">{formatMaybe(d.size_mult, 2)}</td>
                            <td className="py-2 pr-3 text-grey/70">{d.consensus_models || '---'}</td>
                            <td className="py-2 pr-3 text-grey/70">{d.flags || '---'}</td>
                            <td className="py-2 text-grey/70">{d.decision_id.slice(0, 10)}</td>
                          </tr>

                          {expanded ? (
                            <tr className="border-b border-grey/10">
                              <td colSpan={11} className="py-3">
                                {llmModelsLoading[d.decision_id] ? (
                                  <div className="text-[11px] font-mono text-grey/70">LOADING MODELS…</div>
                                ) : (
                                  <div className="space-y-3">
                                    {(llmModelsByDecision[d.decision_id] || []).map((m: VaultLlmModelRecordRow) => (
                                      <div key={m.id} className="border border-grey/20 bg-black/40 p-3">
                                        <div className="flex flex-wrap items-center justify-between gap-2 text-[10px] font-mono">
                                          <div className="text-white">
                                            {m.model} // {m.parsed_ok ? 'PARSED_OK' : 'PARSE_FAIL'}
                                          </div>
                                          <div className="text-grey/70">
                                            LAT {m.latency_ms ?? '---'}ms // TOK {m.total_tokens ?? '---'}
                                          </div>
                                        </div>
                                        <div className="grid grid-cols-1 md:grid-cols-4 gap-3 mt-2 text-[11px] font-mono">
                                          <div className="text-grey/70">ACTION: <span className="text-white">{m.action || '---'}</span></div>
                                          <div className="text-grey/70">OUTCOME: <span className="text-white">{typeof m.outcome_index === 'number' ? String(m.outcome_index) : '---'}</span></div>
                                          <div className="text-grey/70">P_TRUE: <span className="text-white">{formatMaybe(m.p_true, 3)}</span></div>
                                          <div className="text-grey/70">SIZE: <span className="text-white">{formatMaybe(m.size_mult, 2)}</span></div>
                                        </div>
                                        {m.error ? (
                                          <div className="mt-2 text-[11px] font-mono text-danger">{m.error}</div>
                                        ) : null}
                                        {m.raw_dsl ? (
                                          <pre className="mt-2 text-[10px] font-mono text-grey/80 whitespace-pre-wrap break-words max-h-56 overflow-y-auto bg-black/60 p-2 border border-grey/10">
                                            {m.raw_dsl}
                                          </pre>
                                        ) : null}
                                      </div>
                                    ))}
                                    {Array.isArray(llmModelsByDecision[d.decision_id]) &&
                                    llmModelsByDecision[d.decision_id].length === 0 ? (
                                      <div className="text-[11px] font-mono text-grey/70">NO MODEL RECORDS.</div>
                                    ) : null}
                                  </div>
                                )}
                              </td>
                            </tr>
                          ) : null}
                        </React.Fragment>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            )}
          </Panel>

          <Panel title="FAST15M ATTRIBUTION" right="COMING SOON" className="relative overflow-hidden">
            <Watermark text={paperMode ? 'PAPER' : 'LIVE'} />
            <div className="text-[11px] font-mono text-grey/70">
              Will render p_up inputs (start/now mid, sigma, shrink, orderbook) once stored/exposed.
            </div>
          </Panel>
        </div>
      ) : null}
    </div>
  );
};
