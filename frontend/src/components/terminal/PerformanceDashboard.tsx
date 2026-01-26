import React, { useState, useEffect, useCallback } from 'react';
import { api, PerformanceDashboardResponse, Arb15mResponse, T2TSnapshot, OracleComparisonResponse, WindowResolution, PriceTick as OraclePriceTick } from '../../services/api';

type PerfTab = 'TICK_TO_TRADE' | 'CPU_HOTPATHS' | 'ARB_15M' | 'ORACLE';

// Format microseconds to human readable
function formatUs(us: number): string {
  if (us === 0) return '---';
  if (us < 1000) return `${us}μs`;
  if (us < 1_000_000) return `${(us / 1000).toFixed(1)}ms`;
  return `${(us / 1_000_000).toFixed(2)}s`;
}

function formatCount(n: number): string {
  if (n === 0) return '---';
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(1)}K`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

function tabButtonClass(active: boolean): string {
  return [
    'px-3 py-2 text-[11px] font-mono border transition-colors duration-150',
    active
      ? 'bg-fg text-void border-fg font-semibold'
      : 'border-grey/30 text-fg/90 hover:text-fg hover:border-grey/50 hover:bg-grey/10',
  ].join(' ');
}

// Historical data for tick charts (moved here for component use)
interface LatencyHistoryPoint {
  ts: number;
  t2t_p50_ms: number;
  t2t_p99_ms: number;
  network_p50_ms: number;
  network_p99_ms: number;
  [key: string]: number; // Index signature for dynamic access
}

// Tick chart component for latency visualization
interface LatencyTickChartProps {
  history: LatencyHistoryPoint[];
  valueKey: keyof LatencyHistoryPoint;
  p99Key: keyof LatencyHistoryPoint;
  color: string;
  unit: string;
}

const LatencyTickChart: React.FC<LatencyTickChartProps> = ({ history, valueKey, p99Key, color, unit }) => {
  if (history.length < 2) {
    return (
      <div className="h-24 flex items-center justify-center text-fg/30 text-[10px] font-mono">
        Collecting data...
      </div>
    );
  }

  const values = history.map(h => h[valueKey] as number);
  const p99Values = history.map(h => h[p99Key] as number);
  const allValues = [...values, ...p99Values].filter(v => v > 0);
  
  const minVal = Math.min(...allValues) * 0.9;
  const maxVal = Math.max(...allValues) * 1.1;
  const range = maxVal - minVal || 1;

  const width = 400;
  const height = 80;
  const padding = { left: 45, right: 10, top: 5, bottom: 15 };
  const chartWidth = width - padding.left - padding.right;
  const chartHeight = height - padding.top - padding.bottom;

  const xStep = chartWidth / (history.length - 1);

  // Generate path for p50
  const p50Points = values.map((v, i) => {
    const x = padding.left + i * xStep;
    const y = padding.top + chartHeight - ((v - minVal) / range) * chartHeight;
    return `${i === 0 ? 'M' : 'L'} ${x} ${y}`;
  }).join(' ');

  // Generate path for p99
  const p99Points = p99Values.map((v, i) => {
    const x = padding.left + i * xStep;
    const y = padding.top + chartHeight - ((v - minVal) / range) * chartHeight;
    return `${i === 0 ? 'M' : 'L'} ${x} ${y}`;
  }).join(' ');

  // Y-axis labels
  const yLabels = [maxVal, (maxVal + minVal) / 2, minVal];

  // Format value for axis
  const formatAxisValue = (v: number): string => {
    if (v >= 1000) return `${(v / 1000).toFixed(1)}s`;
    if (v >= 1) return `${v.toFixed(0)}${unit}`;
    return `${(v * 1000).toFixed(0)}μs`;
  };

  // Current values
  const currentP50 = values[values.length - 1];
  const currentP99 = p99Values[p99Values.length - 1];

  return (
    <div className="relative">
      <svg width="100%" height={height} viewBox={`0 0 ${width} ${height}`} className="font-mono">
        {/* Grid lines */}
        {yLabels.map((v, i) => {
          const y = padding.top + (i / (yLabels.length - 1)) * chartHeight;
          return (
            <g key={i}>
              <line x1={padding.left} y1={y} x2={width - padding.right} y2={y} stroke="rgb(var(--c-fg) / 0.1)" />
              <text x={padding.left - 5} y={y + 3} textAnchor="end" fill="rgb(var(--c-fg) / 0.5)" fontSize="8">
                {formatAxisValue(v)}
              </text>
            </g>
          );
        })}

        {/* P99 line (dashed, lighter) */}
        <path d={p99Points} fill="none" stroke={color} strokeWidth="1" strokeDasharray="3,3" opacity="0.4" />

        {/* P50 line (solid) */}
        <path d={p50Points} fill="none" stroke={color} strokeWidth="2" />

        {/* Current value dots */}
        <circle 
          cx={padding.left + (values.length - 1) * xStep} 
          cy={padding.top + chartHeight - ((currentP50 - minVal) / range) * chartHeight}
          r="3"
          fill={color}
        />
      </svg>

      {/* Legend */}
      <div className="absolute top-0 right-0 text-[8px] font-mono flex gap-3">
        <div className="flex items-center gap-1">
          <div className="w-3 h-0.5" style={{ backgroundColor: color }}></div>
          <span className="text-fg/70">p50: {formatAxisValue(currentP50)}</span>
        </div>
        <div className="flex items-center gap-1">
          <div className="w-3 h-0.5 opacity-40" style={{ backgroundColor: color, borderStyle: 'dashed' }}></div>
          <span className="text-fg/50">p99: {formatAxisValue(currentP99)}</span>
        </div>
      </div>
    </div>
  );
};

export const PerformanceDashboard: React.FC = () => {
  const [activeTab, setActiveTab] = useState<PerfTab>('TICK_TO_TRADE');
  const [data, setData] = useState<PerformanceDashboardResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchData = useCallback(async () => {
    try {
      const response = await api.getPerformanceDashboard();
      setData(response);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to fetch');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    fetchData();
    const interval = setInterval(fetchData, 1000); // 1Hz refresh
    return () => clearInterval(interval);
  }, [fetchData]);

  return (
    <div className="h-full flex flex-col bg-void text-fg overflow-hidden">
      {/* Sub-navigation */}
      <div className="px-4 py-3 border-b border-grey/40 flex items-center gap-2">
        <span className="text-[11px] font-mono text-fg/70 mr-4">PERFORMANCE</span>
        <div className="flex gap-1">
          <button
            onClick={() => setActiveTab('TICK_TO_TRADE')}
            className={tabButtonClass(activeTab === 'TICK_TO_TRADE')}
          >
            TICK-TO-TRADE
          </button>
          <button
            onClick={() => setActiveTab('CPU_HOTPATHS')}
            className={tabButtonClass(activeTab === 'CPU_HOTPATHS')}
          >
            CPU &amp; HOT PATHS
          </button>
          <button
            onClick={() => setActiveTab('ARB_15M')}
            className={tabButtonClass(activeTab === 'ARB_15M')}
          >
            15M
          </button>
          <button
            onClick={() => setActiveTab('ORACLE')}
            className={tabButtonClass(activeTab === 'ORACLE')}
          >
            ORACLE
          </button>
        </div>
      </div>

      {/* Tab Content */}
      <div className="flex-1 overflow-auto p-4">
        {loading && !data && (
          <div className="flex items-center justify-center h-full text-fg/50 font-mono text-[12px]">
            Loading performance data...
          </div>
        )}
        {error && !data && (
          <div className="flex items-center justify-center h-full text-danger font-mono text-[12px]">
            {error}
          </div>
        )}
        {activeTab === 'TICK_TO_TRADE' && <TickToTradeTab data={data} />}
        {activeTab === 'CPU_HOTPATHS' && <CpuHotPathsTab data={data} />}
        {activeTab === 'ARB_15M' && <Arb15mTab />}
        {activeTab === 'ORACLE' && <OracleComparisonTab />}
      </div>
    </div>
  );
};

const TickToTradeTab: React.FC<{ data: PerformanceDashboardResponse | null }> = ({ data }) => {
  // Use comprehensive T2T breakdown if available
  const comp = data?.comprehensive;
  const t2t = comp?.t2t;
  const throughput = comp?.throughput;
  const mdIntegrity = comp?.md_integrity || [];
  const jitter = t2t?.jitter;
  const failures = comp?.failures;
  const orderLifecycle = comp?.order_lifecycle;

  // Track historical latency for tick charts (last 60 samples = 60 seconds at 1Hz)
  const [latencyHistory, setLatencyHistory] = useState<LatencyHistoryPoint[]>([]);
  
  // Update history when data changes
  useEffect(() => {
    if (t2t?.total?.p50_us !== undefined) {
      setLatencyHistory(prev => {
        const newPoint: LatencyHistoryPoint = {
          ts: Date.now(),
          t2t_p50_ms: (t2t.total?.p50_us || 0) / 1000,
          t2t_p99_ms: (t2t.total?.p99_us || 0) / 1000,
          network_p50_ms: (t2t.md_receive?.p50_us || 0) / 1000,
          network_p99_ms: (t2t.md_receive?.p99_us || 0) / 1000,
        };
        const updated = [...prev, newPoint].slice(-60); // Keep last 60 samples
        return updated;
      });
    }
  }, [t2t?.total?.p50_us, t2t?.total?.p99_us, t2t?.md_receive?.p50_us, t2t?.md_receive?.p99_us]);

  const getStatus = (p99: number | undefined): 'idle' | 'good' | 'warn' | 'bad' => {
    if (!p99 || p99 === 0) return 'idle';
    if (p99 < 1000) return 'good';
    if (p99 < 10000) return 'warn';
    return 'bad';
  };

  // Determine status for total T2T (in milliseconds)
  const getTotalStatus = (p50_us: number | undefined): 'idle' | 'good' | 'warn' | 'bad' => {
    if (!p50_us || p50_us === 0) return 'idle';
    const p50_ms = p50_us / 1000;
    if (p50_ms < 100) return 'good';
    if (p50_ms < 500) return 'warn';
    return 'bad';
  };

  const getNetworkStatus = (p50_us: number | undefined): 'idle' | 'good' | 'warn' | 'bad' => {
    if (!p50_us || p50_us === 0) return 'idle';
    const p50_ms = p50_us / 1000;
    if (p50_ms < 50) return 'good';
    if (p50_ms < 200) return 'warn';
    return 'bad';
  };

  // Calculate component breakdown percentages
  const mdReceive = t2t?.md_receive?.p50_us || 0;
  const mdDecode = t2t?.md_decode?.p50_us || 0;
  const signalCompute = t2t?.signal_compute?.p50_us || 0;
  const riskCheck = t2t?.risk_check?.p50_us || 0;
  const orderBuild = t2t?.order_build?.p50_us || 0;
  const wireSend = t2t?.wire_send?.p50_us || 0;
  
  const componentSum = mdReceive + mdDecode + signalCompute + riskCheck + orderBuild + wireSend;
  const hasBreakdown = componentSum > 0;

  return (
    <div className="space-y-6">
      {/* Header with refresh indicator */}
      <div className="border-b border-grey/40 pb-4 flex justify-between items-start">
        <div>
          <h2 className="text-[14px] font-mono font-semibold text-fg">TICK-TO-TRADE LATENCY</h2>
          <p className="text-[11px] font-mono text-fg/90 mt-1">
            End-to-end latency from market data reception to order submission
          </p>
        </div>
        <div className="text-[10px] font-mono text-fg/50 flex items-center gap-2">
          <span className="w-2 h-2 bg-green-500 rounded-full animate-pulse"></span>
          LIVE (1Hz)
        </div>
      </div>

      {/* HEADLINE METRICS - Total T2T and Network Latency */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <div className="border-2 border-better-blue/50 bg-better-blue/5 p-4">
          <div className="text-[10px] font-mono text-better-blue mb-1">TOTAL TICK-TO-TRADE (p50)</div>
          <div className={`font-mono font-bold text-[32px] ${
            getTotalStatus(t2t?.total?.p50_us) === 'good' ? 'text-success' :
            getTotalStatus(t2t?.total?.p50_us) === 'warn' ? 'text-warning' :
            getTotalStatus(t2t?.total?.p50_us) === 'bad' ? 'text-danger' : 'text-fg/50'
          }`}>
            {t2t?.total?.p50_us ? formatUs(t2t.total.p50_us) : '---'}
          </div>
          <div className="mt-2 text-[10px] font-mono text-fg/70 grid grid-cols-3 gap-2">
            <div>
              <span className="text-fg/50">p50:</span>
              <span className="ml-1 text-fg">{formatUs(t2t?.total?.p50_us || 0)}</span>
            </div>
            <div>
              <span className="text-fg/50">p99:</span>
              <span className="ml-1 text-fg">{formatUs(t2t?.total?.p99_us || 0)}</span>
            </div>
            <div>
              <span className="text-fg/50">samples:</span>
              <span className="ml-1 text-fg">{formatCount(t2t?.total?.count || 0)}</span>
            </div>
          </div>
        </div>

        <div className="border-2 border-cyan-500/50 bg-cyan-500/5 p-4">
          <div className="text-[10px] font-mono text-cyan-400 mb-1">NETWORK LATENCY (p50)</div>
          <div className={`font-mono font-bold text-[32px] ${
            getNetworkStatus(t2t?.md_receive?.p50_us) === 'good' ? 'text-success' :
            getNetworkStatus(t2t?.md_receive?.p50_us) === 'warn' ? 'text-warning' :
            getNetworkStatus(t2t?.md_receive?.p50_us) === 'bad' ? 'text-danger' : 'text-fg/50'
          }`}>
            {t2t?.md_receive?.p50_us ? formatUs(t2t.md_receive.p50_us) : '---'}
          </div>
          <div className="mt-2 text-[10px] font-mono text-fg/70 grid grid-cols-3 gap-2">
            <div>
              <span className="text-fg/50">p50:</span>
              <span className="ml-1 text-fg">{formatUs(t2t?.md_receive?.p50_us || 0)}</span>
            </div>
            <div>
              <span className="text-fg/50">p99:</span>
              <span className="ml-1 text-fg">{formatUs(t2t?.md_receive?.p99_us || 0)}</span>
            </div>
            <div>
              <span className="text-fg/50">samples:</span>
              <span className="ml-1 text-fg">{formatCount(t2t?.md_receive?.count || 0)}</span>
            </div>
          </div>
        </div>
      </div>

      {/* TICK CHARTS - Real-time latency history */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <div className="border border-grey/40 p-4">
          <div className="text-[10px] font-mono text-fg/70 mb-2">T2T LATENCY (last 60s)</div>
          <LatencyTickChart 
            history={latencyHistory} 
            valueKey="t2t_p50_ms" 
            p99Key="t2t_p99_ms"
            color="#3B82F6" 
            unit="ms"
          />
        </div>
        <div className="border border-grey/40 p-4">
          <div className="text-[10px] font-mono text-fg/70 mb-2">NETWORK LATENCY (last 60s)</div>
          <LatencyTickChart 
            history={latencyHistory} 
            valueKey="network_p50_ms" 
            p99Key="network_p99_ms"
            color="#06B6D4" 
            unit="ms"
          />
        </div>
      </div>

      {/* DETAILED COMPONENT BREAKDOWN */}
      <div className="border border-grey/40 p-4">
        <div className="text-[12px] font-mono font-semibold text-fg/90 mb-4">LATENCY COMPONENT BREAKDOWN</div>
        {hasBreakdown ? (
          <>
            {/* Stacked bar visualization */}
            <div className="h-8 bg-grey/10 flex overflow-hidden rounded mb-4">
              <div 
                className="bg-cyan-500 h-full flex items-center justify-center text-[8px] font-mono text-black font-semibold"
                style={{ width: `${(mdReceive / componentSum) * 100}%`, minWidth: mdReceive > 0 ? '30px' : '0' }}
                title={`Network: ${formatUs(mdReceive)}`}
              >
                {(mdReceive / componentSum) * 100 > 8 && 'NET'}
              </div>
              <div 
                className="bg-blue-500 h-full flex items-center justify-center text-[8px] font-mono text-black font-semibold"
                style={{ width: `${(mdDecode / componentSum) * 100}%`, minWidth: mdDecode > 0 ? '20px' : '0' }}
                title={`Decode: ${formatUs(mdDecode)}`}
              >
                {(mdDecode / componentSum) * 100 > 8 && 'DEC'}
              </div>
              <div 
                className="bg-yellow-500 h-full flex items-center justify-center text-[8px] font-mono text-black font-semibold"
                style={{ width: `${(signalCompute / componentSum) * 100}%`, minWidth: signalCompute > 0 ? '20px' : '0' }}
                title={`Signal: ${formatUs(signalCompute)}`}
              >
                {(signalCompute / componentSum) * 100 > 8 && 'SIG'}
              </div>
              <div 
                className="bg-orange-500 h-full flex items-center justify-center text-[8px] font-mono text-black font-semibold"
                style={{ width: `${(riskCheck / componentSum) * 100}%`, minWidth: riskCheck > 0 ? '20px' : '0' }}
                title={`Risk: ${formatUs(riskCheck)}`}
              >
                {(riskCheck / componentSum) * 100 > 8 && 'RSK'}
              </div>
              <div 
                className="bg-red-500 h-full flex items-center justify-center text-[8px] font-mono text-black font-semibold"
                style={{ width: `${(orderBuild / componentSum) * 100}%`, minWidth: orderBuild > 0 ? '20px' : '0' }}
                title={`Build: ${formatUs(orderBuild)}`}
              >
                {(orderBuild / componentSum) * 100 > 8 && 'BLD'}
              </div>
              <div 
                className="bg-green-500 h-full flex items-center justify-center text-[8px] font-mono text-black font-semibold"
                style={{ width: `${(wireSend / componentSum) * 100}%`, minWidth: wireSend > 0 ? '30px' : '0' }}
                title={`Wire: ${formatUs(wireSend)}`}
              >
                {(wireSend / componentSum) * 100 > 8 && 'WIRE'}
              </div>
            </div>

            {/* Detailed breakdown table */}
            <div className="grid grid-cols-2 md:grid-cols-3 lg:grid-cols-6 gap-3 text-[10px] font-mono">
              <div className="bg-cyan-500/10 border border-cyan-500/30 p-2 rounded">
                <div className="flex items-center gap-1 mb-1">
                  <div className="w-2 h-2 bg-cyan-500 rounded"></div>
                  <span className="text-cyan-400">NETWORK</span>
                </div>
                <div className="text-fg font-semibold">{formatUs(mdReceive)}</div>
                <div className="text-fg/50">{((mdReceive / componentSum) * 100).toFixed(1)}%</div>
              </div>
              <div className="bg-blue-500/10 border border-blue-500/30 p-2 rounded">
                <div className="flex items-center gap-1 mb-1">
                  <div className="w-2 h-2 bg-blue-500 rounded"></div>
                  <span className="text-blue-400">DECODE</span>
                </div>
                <div className="text-fg font-semibold">{formatUs(mdDecode)}</div>
                <div className="text-fg/50">{((mdDecode / componentSum) * 100).toFixed(1)}%</div>
              </div>
              <div className="bg-yellow-500/10 border border-yellow-500/30 p-2 rounded">
                <div className="flex items-center gap-1 mb-1">
                  <div className="w-2 h-2 bg-yellow-500 rounded"></div>
                  <span className="text-yellow-400">SIGNAL</span>
                </div>
                <div className="text-fg font-semibold">{formatUs(signalCompute)}</div>
                <div className="text-fg/50">{((signalCompute / componentSum) * 100).toFixed(1)}%</div>
              </div>
              <div className="bg-orange-500/10 border border-orange-500/30 p-2 rounded">
                <div className="flex items-center gap-1 mb-1">
                  <div className="w-2 h-2 bg-orange-500 rounded"></div>
                  <span className="text-orange-400">RISK</span>
                </div>
                <div className="text-fg font-semibold">{formatUs(riskCheck)}</div>
                <div className="text-fg/50">{((riskCheck / componentSum) * 100).toFixed(1)}%</div>
              </div>
              <div className="bg-red-500/10 border border-red-500/30 p-2 rounded">
                <div className="flex items-center gap-1 mb-1">
                  <div className="w-2 h-2 bg-red-500 rounded"></div>
                  <span className="text-red-400">BUILD</span>
                </div>
                <div className="text-fg font-semibold">{formatUs(orderBuild)}</div>
                <div className="text-fg/50">{((orderBuild / componentSum) * 100).toFixed(1)}%</div>
              </div>
              <div className="bg-green-500/10 border border-green-500/30 p-2 rounded">
                <div className="flex items-center gap-1 mb-1">
                  <div className="w-2 h-2 bg-green-500 rounded"></div>
                  <span className="text-green-400">WIRE</span>
                </div>
                <div className="text-fg font-semibold">{formatUs(wireSend)}</div>
                <div className="text-fg/50">{((wireSend / componentSum) * 100).toFixed(1)}%</div>
              </div>
            </div>
          </>
        ) : (
          <div className="text-center text-fg/50 py-8">
            No latency breakdown data - start paper trading to collect metrics
          </div>
        )}
      </div>

      {/* Stage Breakdown Cards */}
      <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
        <MetricCard
          label="SIGNAL COMPUTE"
          value={formatUs(t2t?.signal_compute?.p50_us || 0)}
          unit=""
          sublabel="p50 / p99"
          subvalue={`${formatUs(t2t?.signal_compute?.p50_us || 0)} / ${formatUs(t2t?.signal_compute?.p99_us || 0)}`}
          status={getStatus(t2t?.signal_compute?.p99_us)}
        />
        <MetricCard
          label="RISK CHECK"
          value={formatUs(t2t?.risk_check?.p50_us || 0)}
          unit=""
          sublabel="p50 / p99"
          subvalue={`${formatUs(t2t?.risk_check?.p50_us || 0)} / ${formatUs(t2t?.risk_check?.p99_us || 0)}`}
          status={getStatus(t2t?.risk_check?.p99_us)}
        />
        <MetricCard
          label="ORDER BUILD"
          value={formatUs(t2t?.order_build?.p50_us || 0)}
          unit=""
          sublabel="p50 / p99"
          subvalue={`${formatUs(t2t?.order_build?.p50_us || 0)} / ${formatUs(t2t?.order_build?.p99_us || 0)}`}
          status={getStatus(t2t?.order_build?.p99_us)}
        />
        <MetricCard
          label="WIRE SEND"
          value={formatUs(t2t?.wire_send?.p50_us || 0)}
          unit=""
          sublabel="p50 / p99"
          subvalue={`${formatUs(t2t?.wire_send?.p50_us || 0)} / ${formatUs(t2t?.wire_send?.p99_us || 0)}`}
          status={getStatus(t2t?.wire_send?.p99_us)}
        />
      </div>

      {/* Throughput Metrics */}
      <div className="border border-grey/40 p-4">
        <h3 className="text-[12px] font-mono font-semibold text-fg/90 mb-4">THROUGHPUT (per sec)</h3>
        <div className="grid grid-cols-2 md:grid-cols-4 lg:grid-cols-6 gap-4">
          <MetricCard
            label="MD MSGS"
            value={throughput?.md_messages_per_sec?.toFixed(1) || '---'}
            unit="/s"
            sublabel=""
            subvalue=""
            status={throughput?.md_messages_per_sec ? 'good' : 'idle'}
            compact
          />
          <MetricCard
            label="SIGNALS"
            value={throughput?.signals_per_sec?.toFixed(1) || '---'}
            unit="/s"
            sublabel=""
            subvalue=""
            status={throughput?.signals_per_sec ? 'good' : 'idle'}
            compact
          />
          <MetricCard
            label="ORDERS"
            value={throughput?.orders_per_sec?.toFixed(1) || '---'}
            unit="/s"
            sublabel=""
            subvalue=""
            status={throughput?.orders_per_sec ? 'good' : 'idle'}
            compact
          />
          <MetricCard
            label="DB WRITES"
            value={throughput?.db_writes_per_sec?.toFixed(1) || '---'}
            unit="/s"
            sublabel=""
            subvalue=""
            status={throughput?.db_writes_per_sec ? 'good' : 'idle'}
            compact
          />
          <MetricCard
            label="FILL RATE"
            value={throughput?.fill_rate_pct?.toFixed(1) || '---'}
            unit="%"
            sublabel=""
            subvalue=""
            status={throughput?.fill_rate_pct ? (throughput.fill_rate_pct > 80 ? 'good' : 'warn') : 'idle'}
            compact
          />
          <MetricCard
            label="REJECT RATE"
            value={throughput?.reject_rate_pct?.toFixed(1) || '---'}
            unit="%"
            sublabel=""
            subvalue=""
            status={throughput?.reject_rate_pct ? (throughput.reject_rate_pct < 5 ? 'good' : 'bad') : 'idle'}
            compact
          />
        </div>
      </div>

      {/* Jitter & MD Integrity */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <div className="border border-grey/40 p-4">
          <h3 className="text-[12px] font-mono font-semibold text-fg/90 mb-4">JITTER</h3>
          <div className="grid grid-cols-2 gap-4">
            <MetricCard
              label="STDDEV"
              value={formatUs(jitter?.stddev_us || 0)}
              unit=""
              sublabel="Latency variance"
              subvalue={`${formatCount(jitter?.sample_count || 0)} samples`}
              status={jitter?.stddev_us && jitter.stddev_us > 1000 ? 'warn' : jitter?.stddev_us ? 'good' : 'idle'}
              compact
            />
            <MetricCard
              label="SPIKES"
              value={String(jitter?.spike_count || 0)}
              unit=""
              sublabel="Above 2x mean"
              subvalue={`${(jitter?.spike_rate_pct || 0).toFixed(2)}%`}
              status={jitter?.spike_rate_pct && jitter.spike_rate_pct > 1 ? 'bad' : jitter?.spike_count ? 'good' : 'idle'}
              compact
            />
          </div>
        </div>

        <div className="border border-grey/40 p-4">
          <h3 className="text-[12px] font-mono font-semibold text-fg/90 mb-4">MD INTEGRITY</h3>
          <div className="space-y-2 text-[11px] font-mono">
            {mdIntegrity.length === 0 ? (
              <div className="text-fg/60">No data sources</div>
            ) : (
              mdIntegrity.map(src => (
                <div key={src.source} className="flex justify-between items-center border-b border-grey/20 pb-1">
                  <span className="text-fg/70">{src.source}</span>
                  <div className="flex gap-4">
                    <span className="text-fg">{formatCount(src.messages)} msgs</span>
                    <span className={src.gaps > 0 ? 'text-danger' : 'text-green'}>{src.gaps} gaps</span>
                    <span className={src.out_of_order > 0 ? 'text-warn' : 'text-green'}>{src.out_of_order} OOO</span>
                  </div>
                </div>
              ))
            )}
          </div>
        </div>
      </div>

      {/* Order Lifecycle & Failures */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <div className="border border-grey/40 p-4">
          <h3 className="text-[12px] font-mono font-semibold text-fg/90 mb-4">ORDER LIFECYCLE</h3>
          <div className="grid grid-cols-3 gap-2 text-[11px] font-mono">
            <div className="text-center p-2 bg-grey/10">
              <div className="text-fg font-semibold">{orderLifecycle?.orders_sent || 0}</div>
              <div className="text-fg/80">SENT</div>
            </div>
            <div className="text-center p-2 bg-grey/10">
              <div className="text-green font-semibold">{orderLifecycle?.orders_filled || 0}</div>
              <div className="text-fg/80">FILLED</div>
            </div>
            <div className="text-center p-2 bg-grey/10">
              <div className={orderLifecycle?.orders_rejected ? 'text-danger font-semibold' : 'text-fg font-semibold'}>
                {orderLifecycle?.orders_rejected || 0}
              </div>
              <div className="text-fg/80">REJECTED</div>
            </div>
          </div>
          <div className="mt-2 text-[10px] font-mono text-fg/70">
            Fill rate: {(orderLifecycle?.fill_rate_pct || 0).toFixed(1)}% | Reject rate: {(orderLifecycle?.reject_rate_pct || 0).toFixed(1)}%
          </div>
        </div>

        <div className="border border-grey/40 p-4">
          <h3 className="text-[12px] font-mono font-semibold text-fg/90 mb-4">FAILURES / RECOVERY</h3>
          <div className="grid grid-cols-3 gap-2 text-[11px] font-mono">
            <div className="text-center p-2 bg-grey/10">
              <div className={failures?.reconnects ? 'text-warn font-semibold' : 'text-fg font-semibold'}>
                {failures?.reconnects || 0}
              </div>
              <div className="text-fg/80">RECONNECTS</div>
            </div>
            <div className="text-center p-2 bg-grey/10">
              <div className={failures?.circuit_breaker_trips ? 'text-danger font-semibold' : 'text-fg font-semibold'}>
                {failures?.circuit_breaker_trips || 0}
              </div>
              <div className="text-fg/80">CB TRIPS</div>
            </div>
            <div className="text-center p-2 bg-grey/10">
              <div className="text-fg font-semibold">{formatUs(failures?.recovery_time?.p99_us || 0)}</div>
              <div className="text-fg/80">RECOV p99</div>
            </div>
          </div>
        </div>
      </div>

      {/* Network & Colocation Section */}
      <div className="border border-grey/40 p-4">
        <h3 className="text-[12px] font-mono font-semibold text-fg/90 mb-4">NETWORK &amp; COLOCATION</h3>
        <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
          <MetricCard
            label="DATACENTER RTT"
            value="---"
            unit="ms"
            sublabel="To exchange"
            subvalue="---"
            status="idle"
            compact
          />
          <MetricCard
            label="COLOCATION"
            value="---"
            unit=""
            sublabel="Status"
            subvalue="NOT CONFIGURED"
            status="idle"
            compact
          />
          <MetricCard
            label="KERNEL BYPASS"
            value="---"
            unit=""
            sublabel="DPDK / io_uring"
            subvalue="DISABLED"
            status="idle"
            compact
          />
        </div>
      </div>

      {/* Latency Breakdown Timeline */}
      <div className="border border-grey/40 p-4">
        <h3 className="text-[12px] font-mono font-semibold text-fg/90 mb-4">LATENCY BREAKDOWN</h3>
        <LatencyBreakdownChart t2t={t2t} />
      </div>

      {/* Percentile Distribution */}
      <div className="border border-grey/40 p-4">
        <h3 className="text-[12px] font-mono font-semibold text-fg/90 mb-4">PERCENTILE DISTRIBUTION</h3>
        <PercentileHistogram t2t={t2t} />
      </div>

      {/* Recent Samples Table */}
      <div className="border border-grey/40 p-4">
        <h3 className="text-[12px] font-mono font-semibold text-fg/90 mb-4">RECENT SAMPLES</h3>
        <LatencySamplesTable t2t={t2t} />
      </div>
    </div>
  );
};

const CpuHotPathsTab: React.FC<{ data: PerformanceDashboardResponse | null }> = ({ data }) => {
  const cpu = data?.cpu;
  const throughput = data?.throughput;
  const counters = data?.latency?.counters;

  // Use new fields from backend
  const cpuPct = cpu?.cpu_utilization_pct || 0;
  const uptimeSecs = (cpu?.uptime_us || 0) / 1_000_000;
  const topSpans = cpu?.top_spans || [];
  const hotPaths = cpu?.hot_paths || [];
  
  // Get rates from throughput (use recent_rates for live data, lifetime_rates for averages)
  const rates = throughput?.recent_rates || throughput?.lifetime_rates;
  const totals = throughput?.totals;

  return (
    <div className="space-y-6">
      {/* Header */}
      <div className="border-b border-grey/40 pb-4">
        <h2 className="text-[14px] font-mono font-semibold text-fg">CPU TIME &amp; HOT PATHS</h2>
        <p className="text-[11px] font-mono text-fg/90 mt-1">
          Function-level profiling, core utilization, and throughput metrics
        </p>
      </div>

      {/* CPU Overview */}
      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4">
        <MetricCard
          label="CPU UTILIZATION"
          value={cpuPct > 0 ? `${cpuPct.toFixed(1)}` : '---'}
          unit="%"
          sublabel="Uptime"
          subvalue={uptimeSecs > 0 ? `${(uptimeSecs / 60).toFixed(1)}m` : '---'}
          status={cpuPct > 80 ? 'bad' : cpuPct > 50 ? 'warn' : cpuPct > 0 ? 'good' : 'idle'}
        />
        <MetricCard
          label="SPAN COUNT"
          value={String(cpu?.span_count || '---')}
          unit=""
          sublabel="Traced spans"
          subvalue={`${formatUs(cpu?.total_cpu_us || 0)} total`}
          status={cpu?.span_count ? 'good' : 'idle'}
        />
        <MetricCard
          label="SIGNALS"
          value={rates?.signals_per_sec ? `${rates.signals_per_sec.toFixed(1)}` : '---'}
          unit="/s"
          sublabel="Total detected"
          subvalue={`${formatCount(totals?.signals_detected || counters?.signals_detected || 0)}`}
          status={rates?.signals_per_sec ? 'good' : 'idle'}
        />
        <MetricCard
          label="BINANCE FEED"
          value={rates?.binance_per_sec ? `${rates.binance_per_sec.toFixed(0)}` : '---'}
          unit="/s"
          sublabel="Total updates"
          subvalue={`${formatCount(totals?.binance_updates || counters?.binance_updates || 0)}`}
          status={rates?.binance_per_sec ? 'good' : 'idle'}
        />
      </div>

      {/* Hot Spans Table */}
      <div className="border border-grey/40 p-4">
        <h3 className="text-[12px] font-mono font-semibold text-fg/90 mb-4">TOP SPANS (by CPU time)</h3>
        {topSpans.length > 0 ? (
          <div className="overflow-x-auto">
            <table className="w-full text-[11px] font-mono">
              <thead>
                <tr className="text-left text-fg/60 border-b border-grey/30">
                  <th className="pb-2 pr-4">SPAN</th>
                  <th className="pb-2 pr-4 text-right">TOTAL</th>
                  <th className="pb-2 pr-4 text-right">COUNT</th>
                  <th className="pb-2 pr-4 text-right">AVG</th>
                  <th className="pb-2 text-right">% CPU</th>
                </tr>
              </thead>
              <tbody>
                {topSpans.map((span, i) => {
                  const totalUs = span.total_time_us || span.total_us || 0;
                  const count = span.invocations || span.count || 0;
                  const avgUs = count > 0 ? totalUs / count : 0;
                  const totalCpuUs = cpu?.total_cpu_us || 1;
                  const pctOfTotal = (totalUs / totalCpuUs) * 100;
                  return (
                    <tr key={i} className="border-b border-grey/10 hover:bg-grey/5">
                      <td className="py-2 pr-4 text-fg">{span.name}</td>
                      <td className="py-2 pr-4 text-right text-fg/80">{formatUs(totalUs)}</td>
                      <td className="py-2 pr-4 text-right text-fg/80">{formatCount(count)}</td>
                      <td className="py-2 pr-4 text-right text-fg/80">{formatUs(avgUs)}</td>
                      <td className="py-2 text-right">
                        <span className={pctOfTotal > 50 ? 'text-danger' : pctOfTotal > 20 ? 'text-warning' : 'text-success'}>
                          {pctOfTotal.toFixed(1)}%
                        </span>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        ) : (
          <div className="text-center text-fg/50 py-4">No span data collected yet</div>
        )}
      </div>

      {/* Hot Paths */}
      <div className="border border-grey/40 p-4">
        <h3 className="text-[12px] font-mono font-semibold text-fg/90 mb-4">HOT PATHS</h3>
        {hotPaths.length > 0 ? (
          <div className="space-y-2">
            {hotPaths.map((path, i) => (
              <div key={i} className="flex items-center justify-between p-2 bg-grey/5 hover:bg-grey/10">
                <span className="text-fg/80 text-[10px] font-mono truncate max-w-[60%]">{path.path}</span>
                <div className="flex items-center gap-4 text-[10px] font-mono">
                  <span className="text-fg/60">{formatCount(path.count)} calls</span>
                  <span className="text-fg/80">{formatUs(path.avg_us)} avg</span>
                  <span className={path.pct_of_total > 30 ? 'text-danger' : 'text-success'}>
                    {path.pct_of_total.toFixed(1)}%
                  </span>
                </div>
              </div>
            ))}
          </div>
        ) : (
          <div className="text-center text-fg/50 py-4">No hot path data - enable profiling to collect</div>
        )}
      </div>

      {/* Throughput Details */}
      <div className="border border-grey/40 p-4">
        <h3 className="text-[12px] font-mono font-semibold text-fg/90 mb-4">THROUGHPUT BREAKDOWN</h3>
        <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
          <MetricCard
            label="DOME WS"
            value={rates?.dome_ws_per_sec ? `${rates.dome_ws_per_sec.toFixed(1)}` : '---'}
            unit="/s"
            sublabel="Total events"
            subvalue={formatCount(totals?.dome_ws_events || 0)}
            status={rates?.dome_ws_per_sec ? 'good' : 'idle'}
            compact
          />
          <MetricCard
            label="API REQUESTS"
            value={rates?.api_per_sec ? `${rates.api_per_sec.toFixed(1)}` : '---'}
            unit="/s"
            sublabel="Total"
            subvalue={formatCount(totals?.api_requests || 0)}
            status={rates?.api_per_sec ? 'good' : 'idle'}
            compact
          />
          <MetricCard
            label="WS MESSAGES"
            value={rates?.ws_messages_per_sec ? `${rates.ws_messages_per_sec.toFixed(1)}` : '---'}
            unit="/s"
            sublabel="Sent"
            subvalue={formatCount(totals?.ws_messages_sent || 0)}
            status={rates?.ws_messages_per_sec ? 'good' : 'idle'}
            compact
          />
          <MetricCard
            label="TRADES"
            value={rates?.trades_per_sec ? `${rates.trades_per_sec.toFixed(1)}` : '---'}
            unit="/s"
            sublabel="Executed"
            subvalue={formatCount(totals?.trades_executed || 0)}
            status={rates?.trades_per_sec ? 'good' : 'idle'}
            compact
          />
        </div>
      </div>

      {/* Core Affinity Map */}
      <div className="border border-grey/40 p-4">
        <h3 className="text-[12px] font-mono font-semibold text-fg/90 mb-4">CORE AFFINITY MAP</h3>
        <CoreAffinityMap />
      </div>

      {/* Memory & Allocations */}
      <div className="border border-grey/40 p-4">
        <h3 className="text-[12px] font-mono font-semibold text-fg/90 mb-4">MEMORY &amp; ALLOCATIONS</h3>
        <MemoryMetrics data={data} />
      </div>
    </div>
  );
};

interface MetricCardProps {
  label: string;
  value: string;
  unit: string;
  sublabel: string;
  subvalue: string;
  status: 'idle' | 'good' | 'warn' | 'bad';
  compact?: boolean;
}

const MetricCard: React.FC<MetricCardProps> = ({
  label,
  value,
  unit,
  sublabel,
  subvalue,
  status,
  compact = false,
}) => {
  const statusColor = {
    idle: 'text-fg/70',
    good: 'text-success',
    warn: 'text-warning',
    bad: 'text-danger',
  }[status];

  return (
    <div className={`border border-grey/40 ${compact ? 'p-3' : 'p-4'}`}>
      <div className="text-[10px] font-mono text-fg/70 mb-1">{label}</div>
      <div className={`font-mono font-semibold ${compact ? 'text-[18px]' : 'text-[24px]'} ${statusColor}`}>
        {value}
        {unit && <span className="text-[12px] text-fg/50 ml-1">{unit}</span>}
      </div>
      <div className="mt-2 text-[10px] font-mono text-fg/60">
        <span>{sublabel}:</span>
        <span className="ml-1 text-fg/80">{subvalue}</span>
      </div>
    </div>
  );
};

const LatencyBreakdownChart: React.FC<{ t2t?: T2TSnapshot }> = ({ t2t }) => {
  // Calculate stage widths based on actual latency data
  const mdRecv = t2t?.md_receive?.p50_us || 0;
  const mdDecode = t2t?.md_decode?.p50_us || 0;
  const signalCompute = t2t?.signal_compute?.p50_us || 0;
  const riskCheck = t2t?.risk_check?.p50_us || 0;
  const orderBuild = t2t?.order_build?.p50_us || 0;
  const wireSend = t2t?.wire_send?.p50_us || 0;
  
  const total = mdRecv + mdDecode + signalCompute + riskCheck + orderBuild + wireSend;
  const hasData = total > 0;
  
  const stages = [
    { name: 'MD Recv', value: mdRecv, width: hasData ? (mdRecv / total) * 100 : 0, color: 'bg-better-blue' },
    { name: 'Decode', value: mdDecode, width: hasData ? (mdDecode / total) * 100 : 0, color: 'bg-cyan-500' },
    { name: 'Signal', value: signalCompute, width: hasData ? (signalCompute / total) * 100 : 0, color: 'bg-yellow-500' },
    { name: 'Risk', value: riskCheck, width: hasData ? (riskCheck / total) * 100 : 0, color: 'bg-orange-500' },
    { name: 'Build', value: orderBuild, width: hasData ? (orderBuild / total) * 100 : 0, color: 'bg-red-500' },
    { name: 'Send', value: wireSend, width: hasData ? (wireSend / total) * 100 : 0, color: 'bg-success' },
  ];

  return (
    <div className="space-y-3">
      <div className="h-8 bg-grey/10 flex overflow-hidden">
        {hasData ? (
          stages.map((stage, i) => (
            <div
              key={i}
              className={`${stage.color} h-full flex items-center justify-center text-[9px] font-mono text-black font-semibold`}
              style={{ width: stage.width > 0 ? `${stage.width}%` : '0%', minWidth: stage.width > 0 ? '20px' : '0' }}
            >
              {stage.width > 8 && stage.name}
            </div>
          ))
        ) : (
          <div className="w-full h-full flex items-center justify-center text-[11px] font-mono text-fg/50">
            NO DATA - Start paper trading to view breakdown
          </div>
        )}
      </div>
      <div className="flex flex-wrap gap-4 text-[10px] font-mono">
        {stages.map((stage, i) => (
          <div key={i} className="flex items-center gap-2">
            <div className={`w-3 h-3 ${stage.color}`} />
            <span className="text-fg/70">{stage.name}</span>
            <span className="text-fg/90">{stage.value > 0 ? formatUs(stage.value) : '---'}</span>
          </div>
        ))}
      </div>
      {hasData && (
        <div className="text-[10px] font-mono text-fg/60 mt-2">
          Total p50: {formatUs(total)} | Samples: {formatCount(t2t?.total?.count || 0)}
        </div>
      )}
    </div>
  );
};

const PercentileHistogram: React.FC<{ t2t?: T2TSnapshot }> = ({ t2t }) => {
  // Build histogram from percentile data
  const total = t2t?.total;
  const hasData = total && total.count > 0;
  
  // Estimate distribution from percentiles
  // We'll show percentile markers and their values
  const percentiles = hasData ? [
    { label: 'min', value: total.min_us, pct: 0 },
    { label: 'p50', value: total.p50_us, pct: 50 },
    { label: 'p90', value: total.p90_us, pct: 90 },
    { label: 'p95', value: total.p95_us, pct: 95 },
    { label: 'p99', value: total.p99_us, pct: 99 },
    { label: 'p999', value: total.p999_us, pct: 99.9 },
    { label: 'max', value: total.max_us, pct: 100 },
  ] : [];
  
  const maxValue = hasData ? Math.max(total.max_us, 1) : 1;

  return (
    <div className="space-y-4">
      {hasData ? (
        <>
          {/* Percentile bar chart */}
          <div className="flex items-end h-32 gap-2">
            {percentiles.map((p, i) => (
              <div key={i} className="flex-1 flex flex-col items-center">
                <div
                  className="w-full bg-better-blue/60 transition-all rounded-t"
                  style={{ height: `${Math.max((p.value / maxValue) * 100, 2)}%` }}
                />
                <div className="text-[8px] font-mono text-fg/60 mt-1">{p.label}</div>
                <div className="text-[9px] font-mono text-fg/90">{formatUs(p.value)}</div>
              </div>
            ))}
          </div>
          
          {/* Stats summary */}
          <div className="grid grid-cols-4 gap-2 text-[10px] font-mono">
            <div className="bg-grey/10 p-2 text-center">
              <div className="text-fg/60">Mean</div>
              <div className="text-fg">{formatUs(Math.round(total.mean_us))}</div>
            </div>
            <div className="bg-grey/10 p-2 text-center">
              <div className="text-fg/60">Median</div>
              <div className="text-fg">{formatUs(total.p50_us)}</div>
            </div>
            <div className="bg-grey/10 p-2 text-center">
              <div className="text-fg/60">p99</div>
              <div className="text-fg">{formatUs(total.p99_us)}</div>
            </div>
            <div className="bg-grey/10 p-2 text-center">
              <div className="text-fg/60">Samples</div>
              <div className="text-fg">{formatCount(total.count)}</div>
            </div>
          </div>
        </>
      ) : (
        <div className="h-32 flex items-center justify-center text-[11px] font-mono text-fg/50">
          NO DATA - Start paper trading to view percentile distribution
        </div>
      )}
    </div>
  );
};

const LatencySamplesTable: React.FC<{ t2t?: T2TSnapshot }> = ({ t2t }) => {
  const hasData = t2t && t2t.total?.count > 0;
  
  // Show per-stage breakdown as a table
  const stages = hasData ? [
    { name: 'MD Receive', data: t2t.md_receive },
    { name: 'MD Decode', data: t2t.md_decode },
    { name: 'Signal Compute', data: t2t.signal_compute },
    { name: 'Risk Check', data: t2t.risk_check },
    { name: 'Order Build', data: t2t.order_build },
    { name: 'Wire Send', data: t2t.wire_send },
    { name: 'TOTAL', data: t2t.total },
  ] : [];

  return (
    <div className="overflow-x-auto">
      <table className="w-full text-[11px] font-mono">
        <thead>
          <tr className="border-b border-grey/40">
            <th className="text-left py-2 px-2 text-fg/70 font-normal">STAGE</th>
            <th className="text-right py-2 px-2 text-fg/70 font-normal">COUNT</th>
            <th className="text-right py-2 px-2 text-fg/70 font-normal">MIN</th>
            <th className="text-right py-2 px-2 text-fg/70 font-normal">P50</th>
            <th className="text-right py-2 px-2 text-fg/70 font-normal">P99</th>
            <th className="text-right py-2 px-2 text-fg/70 font-normal">MAX</th>
          </tr>
        </thead>
        <tbody>
          {hasData ? (
            stages.map((stage, i) => (
              <tr key={i} className={`border-b border-grey/20 ${stage.name === 'TOTAL' ? 'bg-grey/10 font-semibold' : ''}`}>
                <td className="py-2 px-2 text-fg/90">{stage.name}</td>
                <td className="py-2 px-2 text-right text-fg/80">{formatCount(stage.data?.count || 0)}</td>
                <td className="py-2 px-2 text-right text-fg/80">{formatUs(stage.data?.min_us || 0)}</td>
                <td className="py-2 px-2 text-right text-success">{formatUs(stage.data?.p50_us || 0)}</td>
                <td className="py-2 px-2 text-right text-warning">{formatUs(stage.data?.p99_us || 0)}</td>
                <td className="py-2 px-2 text-right text-fg/80">{formatUs(stage.data?.max_us || 0)}</td>
              </tr>
            ))
          ) : (
            <tr>
              <td colSpan={6} className="py-8 text-center text-fg/50">
                NO SAMPLES - Start paper trading to collect data
              </td>
            </tr>
          )}
        </tbody>
      </table>
    </div>
  );
};

// Format bytes to human-readable
function formatBytes(bytes: number): string {
  if (bytes === 0) return '---';
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)}MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(1)}GB`;
}

const MemoryMetrics: React.FC<{ data: PerformanceDashboardResponse | null }> = ({ data }) => {
  const mem = data?.memory;
  const sys = mem?.system;
  
  const processResident = sys?.process_resident_bytes || 0;
  const processVirtual = sys?.process_virtual_bytes || 0;
  const totalMem = sys?.total_bytes || 0;
  const usedMem = sys?.used_bytes || 0;
  const availMem = sys?.available_bytes || 0;

  return (
    <div className="grid grid-cols-1 md:grid-cols-4 gap-4">
      <MetricCard
        label="PROCESS RSS"
        value={formatBytes(processResident)}
        unit=""
        sublabel="Resident memory"
        subvalue={processVirtual > 0 ? `Virtual: ${formatBytes(processVirtual)}` : '---'}
        status={processResident > 0 ? 'good' : 'idle'}
        compact
      />
      <MetricCard
        label="SYSTEM TOTAL"
        value={formatBytes(totalMem)}
        unit=""
        sublabel="System RAM"
        subvalue={`Used: ${formatBytes(usedMem)}`}
        status={totalMem > 0 ? 'good' : 'idle'}
        compact
      />
      <MetricCard
        label="AVAILABLE"
        value={formatBytes(availMem)}
        unit=""
        sublabel="Free memory"
        subvalue={totalMem > 0 ? `${((availMem / totalMem) * 100).toFixed(0)}% free` : '---'}
        status={availMem > 0 ? 'good' : 'idle'}
        compact
      />
      <MetricCard
        label="HEAP TRACKED"
        value={formatBytes(mem?.heap_bytes || 0)}
        unit=""
        sublabel="Allocator tracked"
        subvalue={`Peak: ${formatBytes(mem?.peak_heap_bytes || 0)}`}
        status={(mem?.heap_bytes || 0) > 0 ? 'good' : 'idle'}
        compact
      />
    </div>
  );
};

const CoreAffinityMap: React.FC = () => {
  const cores = Array.from({ length: 8 }, (_, i) => ({
    id: i,
    usage: 0,
    pinned: false,
    task: null as string | null,
  }));

  return (
    <div className="space-y-3">
      <div className="grid grid-cols-4 md:grid-cols-8 gap-2">
        {cores.map(core => (
          <div
            key={core.id}
            className={`border ${core.pinned ? 'border-better-blue' : 'border-grey/40'} p-2 text-center`}
          >
            <div className="text-[9px] font-mono text-fg/60">CORE {core.id}</div>
            <div className="text-[14px] font-mono text-fg/70 mt-1">
              {core.usage > 0 ? `${core.usage}%` : '---'}
            </div>
            <div className="text-[8px] font-mono text-fg/50 mt-1 truncate">
              {core.task || 'IDLE'}
            </div>
          </div>
        ))}
      </div>
      <div className="flex gap-4 text-[10px] font-mono text-fg/60">
        <div className="flex items-center gap-2">
          <div className="w-3 h-3 border border-better-blue" />
          <span>Pinned core</span>
        </div>
        <div className="flex items-center gap-2">
          <div className="w-3 h-3 border border-grey/40" />
          <span>Unpinned</span>
        </div>
      </div>
    </div>
  );
};

// ============================================================================
// 15M ARBITRAGE TAB
// ============================================================================

const ASSETS = ['BTC', 'ETH', 'SOL', 'XRP'] as const;
type Asset = typeof ASSETS[number];

const Arb15mTab: React.FC = () => {
  const [asset, setAsset] = useState<Asset>('BTC');
  const [data, setData] = useState<Arb15mResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchData = useCallback(async () => {
    try {
      const response = await api.getArbitrage15m(asset.toLowerCase());
      setData(response);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to fetch');
    } finally {
      setLoading(false);
    }
  }, [asset]);

  useEffect(() => {
    fetchData();
    const interval = setInterval(fetchData, 500); // 2Hz refresh for real-time
    return () => clearInterval(interval);
  }, [fetchData]);

  return (
    <div className="h-full flex flex-col">
      {/* Header with asset selector */}
      <div className="flex items-center justify-between border-b border-grey/40 pb-3 mb-3 flex-shrink-0">
        <div className="flex items-center gap-4">
          <h2 className="text-[14px] font-mono font-semibold text-fg">15M MONITOR</h2>
          <span className="text-[10px] font-mono text-fg/50">Binance vs Polymarket</span>
        </div>
        <div className="flex gap-1">
          {ASSETS.map(a => (
            <button
              key={a}
              onClick={() => setAsset(a)}
              className={[
                'px-3 py-1 text-[11px] font-mono border transition-colors',
                asset === a
                  ? 'bg-better-blue text-fg border-better-blue'
                  : 'border-grey/40 text-fg/90 hover:border-grey/60 hover:text-fg',
              ].join(' ')}
            >
              {a}
            </button>
          ))}
        </div>
      </div>

      {loading && !data && (
        <div className="flex-1 flex items-center justify-center text-fg/50 font-mono text-[12px]">Loading...</div>
      )}
      {error && !data && (
        <div className="flex-1 flex items-center justify-center text-danger font-mono text-[12px]">{error}</div>
      )}

      {data && (
        <div className="flex-1 grid grid-rows-[auto_1fr_1fr] gap-3 min-h-0">
          {/* Row 1: Edge Summary (4 cards) + Signal Banner */}
          <div>
            <div className="grid grid-cols-4 gap-2">
              <EdgeCard label="MODEL P(UP)" value={data.edge.model_p_up} format="pct" />
              <EdgeCard label="MARKET P(UP)" value={data.edge.market_p_up} format="pct" />
              <EdgeCard label="EDGE" value={data.edge.edge_up_bps} format="bps" highlight />
              <EdgeCard label="TIME LEFT" value={data.polymarket.time_remaining_sec} format="time" />
            </div>

          </div>

          {/* Row 2: Price Chart + Orderbook */}
          <div className="grid grid-cols-2 gap-3 min-h-0">
            {/* Binance Price */}
            <div className="border border-grey/40 p-3 flex flex-col min-h-0">
              {(() => {
                const ohlc = data.binance.ohlc_history;
                const tickPct = ohlc.length >= 2 
                  ? ((ohlc[ohlc.length - 1].close - ohlc[0].close) / ohlc[0].close) * 100 
                  : null;
                const startPct = data.binance.start_price && data.binance.mid_price
                  ? ((data.binance.mid_price - data.binance.start_price) / data.binance.start_price) * 100
                  : null;
                return (
                  <>
                    <div className="flex items-center justify-between mb-2 flex-shrink-0">
                      <span className="text-[10px] font-mono text-fg/60">BINANCE {data.binance.symbol}</span>
                      <div className="flex items-baseline gap-2">
                        <span className="text-[16px] font-mono font-semibold text-fg">
                          ${data.binance.mid_price?.toFixed(2) || '---'}
                        </span>
                        {tickPct !== null && (
                          <span className={`text-[10px] font-mono ${tickPct >= 0 ? 'text-green-400' : 'text-red-400'}`}>
                            {tickPct >= 0 ? '+' : ''}{tickPct.toFixed(2)}%
                          </span>
                        )}
                      </div>
                    </div>
                    {/* 15M Start price comparison */}
                    <div className="flex items-center justify-end mb-2 flex-shrink-0 text-[10px] font-mono gap-2">
                      <span className="text-fg/50">${data.binance.start_price?.toFixed(2) || '---'}</span>
                      {startPct !== null && (
                        <span className={startPct >= 0 ? 'text-green-400' : 'text-red-400'}>
                          {startPct >= 0 ? '↑' : '↓'}{Math.abs(startPct).toFixed(3)}%
                        </span>
                      )}
                    </div>
                  </>
                );
              })()}
              <div className="flex-1 min-h-0">
                <PriceChart ohlc={data.binance.ohlc_history} hidePercent />
              </div>
            </div>

            {/* Polymarket Orderbook */}
            <div className="border border-grey/40 p-3 flex flex-col min-h-0">
              <div className="text-[10px] font-mono text-fg/60 mb-2 flex-shrink-0">POLYMARKET ORDERBOOK</div>
              <div className="flex-1 grid grid-cols-2 gap-3 min-h-0">
                <div className="flex flex-col min-h-0">
                  <div className="text-[10px] font-mono text-green-400 mb-1 flex-shrink-0">UP</div>
                  <div className="text-[13px] font-mono text-fg mb-2 flex-shrink-0">
                    {data.polymarket.up_best_bid?.toFixed(2) || '---'} / {data.polymarket.up_best_ask?.toFixed(2) || '---'}
                  </div>
                  <div className="flex-1 min-h-0 overflow-auto">
                    <OrderbookDepth levels={data.polymarket.up_depth} side="bid" />
                  </div>
                </div>
                <div className="flex flex-col min-h-0">
                  <div className="text-[10px] font-mono text-red-400 mb-1 flex-shrink-0">DOWN</div>
                  <div className="text-[13px] font-mono text-fg mb-2 flex-shrink-0">
                    {data.polymarket.down_best_bid?.toFixed(2) || '---'} / {data.polymarket.down_best_ask?.toFixed(2) || '---'}
                  </div>
                  <div className="flex-1 min-h-0 overflow-auto">
                    <OrderbookDepth levels={data.polymarket.down_depth} side="ask" />
                  </div>
                </div>
              </div>
            </div>
          </div>

          {/* Row 3: Latency + Trades */}
          <div className="grid grid-cols-2 gap-3 min-h-0">
            {/* Latency History */}
            <div className="border border-grey/40 p-3 flex flex-col min-h-0">
              <div className="text-[10px] font-mono text-fg/60 mb-2 flex-shrink-0">
                INTERNAL LATENCY
              </div>
              <div className="flex-1 min-h-0">
                <LatencyChart samples={data.binance.latency_history} />
              </div>
            </div>

            {/* Recent Trades */}
            <div className="border border-grey/40 p-3 flex flex-col min-h-0">
              <div className="text-[10px] font-mono text-fg/60 mb-2 flex-shrink-0">
                RECENT TRADES ({data.binance.recent_trades.length})
              </div>
              <div className="flex-1 min-h-0 overflow-auto">
                <TradesList trades={data.binance.recent_trades.slice(-20)} />
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
};

// Sub-components for Arb15mTab

const EdgeCard: React.FC<{
  label: string;
  value: number | null;
  format: 'pct' | 'bps' | 'time';
  highlight?: boolean;
}> = ({ label, value, format }) => {
  let displayValue = '---';
  let colorClass = 'text-fg';
  
  if (value !== null) {
    if (format === 'pct') {
      displayValue = `${(value * 100).toFixed(1)}%`;
    } else if (format === 'bps') {
      displayValue = `${value > 0 ? '+' : ''}${value}bps`;
      colorClass = value > 0 ? 'text-green-400' : value < 0 ? 'text-red-400' : 'text-fg';
    } else if (format === 'time') {
      const mins = Math.floor(value / 60);
      const secs = value % 60;
      displayValue = `${mins}:${secs.toString().padStart(2, '0')}`;
    }
  }

  return (
    <div className="border border-grey/40 p-3">
      <div className="text-[9px] font-mono text-fg/60">{label}</div>
      <div className={`text-[18px] font-mono font-semibold ${colorClass}`}>{displayValue}</div>
    </div>
  );
};

// PriceChart: Full-width responsive chart that fills panel horizontally
// Height is fixed to maintain consistent vertical scale for volatility perception
// Time axis dynamically maps full data range to full horizontal extent
const PriceChart: React.FC<{ ohlc: { ts: number; close: number }[]; hidePercent?: boolean }> = ({ ohlc, hidePercent }) => {
  if (ohlc.length < 2) {
    return (
      <div className="h-full flex items-center justify-center text-fg/50 text-[11px] font-mono">
        Waiting for data...
      </div>
    );
  }

  const prices = ohlc.map(p => p.close);
  const min = Math.min(...prices);
  const max = Math.max(...prices);
  const range = max - min || 1;
  
  // Use percentage-based coordinates for full fill
  const viewWidth = 100;
  const viewHeight = 100;
  const padding = { top: 4, right: 1, bottom: 4, left: 1 };
  const chartWidth = viewWidth - padding.left - padding.right;
  const chartHeight = viewHeight - padding.top - padding.bottom;
  
  const points = ohlc.map((p, i) => {
    const x = padding.left + (i / (ohlc.length - 1)) * chartWidth;
    const y = padding.top + chartHeight - ((p.close - min) / range) * chartHeight;
    return `${x},${y}`;
  }).join(' ');

  const lastPrice = prices[prices.length - 1];
  const firstPrice = prices[0];
  const change = lastPrice - firstPrice;
  const changePct = (change / firstPrice) * 100;
  const color = change >= 0 ? '#22c55e' : '#ef4444';

  return (
    <div className="relative h-full">
      <svg 
        viewBox={`0 0 ${viewWidth} ${viewHeight}`} 
        preserveAspectRatio="none"
        className="w-full h-full"
      >
        <polyline
          points={points}
          fill="none"
          stroke={color}
          strokeWidth="0.8"
          strokeLinecap="round"
          strokeLinejoin="round"
          vectorEffect="non-scaling-stroke"
        />
      </svg>
      {!hidePercent && (
        <div className="absolute bottom-1 right-1 text-[10px] font-mono" style={{ color }}>
          {change >= 0 ? '+' : ''}{changePct.toFixed(2)}%
        </div>
      )}
    </div>
  );
};

const OrderbookDepth: React.FC<{ levels: { price: number; size: number }[]; side: 'bid' | 'ask' }> = ({ levels, side }) => {
  if (levels.length === 0) {
    return <div className="text-[10px] font-mono text-fg/40 mt-2">No depth data</div>;
  }

  const maxSize = Math.max(...levels.map(l => l.size));
  const color = side === 'bid' ? 'bg-green-500/30' : 'bg-red-500/30';
  
  // Split into bids (first half) and asks (second half)
  const half = Math.ceil(levels.length / 2);
  const displayLevels = side === 'bid' ? levels.slice(0, half) : levels.slice(half);

  return (
    <div className="mt-2 space-y-0.5">
      {displayLevels.slice(0, 5).map((level, i) => (
        <div key={i} className="relative h-4">
          <div
            className={`absolute inset-y-0 left-0 ${color}`}
            style={{ width: `${(level.size / maxSize) * 100}%` }}
          />
          <div className="relative flex justify-between text-[9px] font-mono px-1">
            <span className="text-fg/80">{level.price.toFixed(2)}</span>
            <span className="text-fg/60">{level.size.toFixed(0)}</span>
          </div>
        </div>
      ))}
    </div>
  );
};

// LatencyChart: Full-width responsive chart showing total internal latency
// Uses percentile-based Y domain (p2 to p98) so normal variation fills the panel
// Fills entire container height - parent must have explicit height
const LatencyChart: React.FC<{ samples: { ts_ms: number; receive_us: number; propagate_us?: number; process_us?: number; network_us?: number }[] }> = ({ samples }) => {
  if (samples.length < 2) {
    return (
      <div className="h-full flex items-center justify-center text-fg/50 text-[11px] font-mono">
        Collecting latency data...
      </div>
    );
  }

  const values = samples.map(s => s.receive_us);
  const sorted = values.slice().sort((a, b) => a - b);
  
  // Use percentile-based domain so the "normal band" fills the chart
  // p2 to p98 captures 96% of data; outliers clip to edges
  const p2Idx = Math.floor(sorted.length * 0.02);
  const p98Idx = Math.min(Math.floor(sorted.length * 0.98), sorted.length - 1);
  const p2 = sorted[p2Idx] || sorted[0];
  const p98 = sorted[p98Idx] || sorted[sorted.length - 1];
  
  // Add 10% padding to the range
  const rawRange = p98 - p2;
  const paddingAmount = rawRange * 0.1;
  const yMin = Math.max(0, p2 - paddingAmount); // Clamp to positive (latency can't be negative)
  const yMax = p98 + paddingAmount;
  const yRange = yMax - yMin || 1;
  
  // Chart dimensions - percentage-based for full fill
  const viewWidth = 100;
  const viewHeight = 100; // Use full viewBox, SVG will stretch to container
  const margin = { top: 4, right: 1, bottom: 4, left: 1 };
  const innerWidth = viewWidth - margin.left - margin.right;
  const innerHeight = viewHeight - margin.top - margin.bottom;
  
  const getX = (i: number) => margin.left + (i / (samples.length - 1)) * innerWidth;
  
  // Y scale: maps [yMin, yMax] to [innerHeight, 0] (SVG Y is inverted)
  const getY = (value: number) => {
    const clamped = Math.max(yMin, Math.min(yMax, value)); // Clip outliers
    const normalized = (clamped - yMin) / yRange;
    return margin.top + innerHeight * (1 - normalized);
  };
  
  // Build the main line (with clipping for outliers)
  const points = samples.map((s, i) => `${getX(i)},${getY(s.receive_us)}`).join(' ');
  
  // Find outlier positions for markers
  const outlierHigh = samples.map((s, i) => ({ i, v: s.receive_us })).filter(d => d.v > p98);
  const outlierLow = samples.map((s, i) => ({ i, v: s.receive_us })).filter(d => d.v < p2);
  
  // Stats for display
  const avg = values.reduce((a, b) => a + b, 0) / values.length;
  const p50 = sorted[Math.floor(sorted.length * 0.5)] || 0;
  const p99 = sorted[Math.floor(sorted.length * 0.99)] || sorted[sorted.length - 1] || 0;
  const maxVal = sorted[sorted.length - 1] || 0;

  return (
    <div className="h-full flex flex-col">
      {/* Chart area - fills available space */}
      <div className="flex-1 min-h-0">
        <svg 
          viewBox={`0 0 ${viewWidth} ${viewHeight}`} 
          preserveAspectRatio="none"
          className="w-full h-full"
        >
          {/* Main latency trace */}
          <polyline
            points={points}
            fill="none"
            stroke="#60a5fa"
            strokeWidth="0.5"
            strokeLinecap="round"
            strokeLinejoin="round"
            vectorEffect="non-scaling-stroke"
          />
          
          {/* High outlier markers (spikes above p98) */}
          {outlierHigh.map((d, idx) => (
            <g key={`high-${idx}`}>
              <line
                x1={getX(d.i)}
                y1={margin.top}
                x2={getX(d.i)}
                y2={margin.top + 6}
                stroke="#f97316"
                strokeWidth="0.4"
                vectorEffect="non-scaling-stroke"
              />
              <circle
                cx={getX(d.i)}
                cy={margin.top + 2}
                r="1.2"
                fill="#f97316"
              />
            </g>
          ))}
          
          {/* Low outlier markers (below p2 - rare for latency) */}
          {outlierLow.map((d, idx) => (
            <line
              key={`low-${idx}`}
              x1={getX(d.i)}
              y1={viewHeight - margin.bottom - 6}
              x2={getX(d.i)}
              y2={viewHeight - margin.bottom}
              stroke="#22d3ee"
              strokeWidth="0.4"
              vectorEffect="non-scaling-stroke"
            />
          ))}
        </svg>
      </div>
      {/* Stats - fixed height at bottom */}
      <div className="flex-shrink-0 flex justify-between text-[9px] font-mono text-fg/50 pt-1">
        <div className="flex gap-3">
          <span className="text-blue-400">AVG: {formatUs(avg)}</span>
          <span className="text-fg/60">P50: {formatUs(p50)}</span>
        </div>
        <div className="flex gap-3">
          <span className="text-fg/70">P99: {formatUs(p99)}</span>
          <span className="text-fg/50">MAX: {formatUs(maxVal)}</span>
          {outlierHigh.length > 0 && <span className="text-orange-400">▲{outlierHigh.length}</span>}
        </div>
      </div>
    </div>
  );
};

const TradesList: React.FC<{ trades: { ts_ms: number; price: number; size: number; is_buyer_maker: boolean; receive_latency_us: number }[] }> = ({ trades }) => {
  if (trades.length === 0) {
    return <div className="h-full flex items-center justify-center text-[10px] font-mono text-fg/40">No recent trades</div>;
  }

  return (
    <table className="w-full text-[10px] font-mono">
      <thead className="text-fg/60 sticky top-0 bg-void">
        <tr>
          <th className="text-left py-1">TIME</th>
          <th className="text-right">PRICE</th>
          <th className="text-right">SIZE</th>
          <th className="text-right">LAT</th>
        </tr>
      </thead>
      <tbody>
        {trades.slice().reverse().map((t, i) => (
          <tr key={i} className={t.is_buyer_maker ? 'text-red-400' : 'text-green-400'}>
            <td className="py-0.5">{new Date(t.ts_ms).toLocaleTimeString()}</td>
            <td className="text-right">{t.price.toFixed(2)}</td>
            <td className="text-right">{t.size.toFixed(4)}</td>
            <td className="text-right text-fg/60">{formatUs(t.receive_latency_us)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
};

// ============================================================================
// ORACLE COMPARISON TAB (Chainlink vs Binance)
// ============================================================================

const ORACLE_ASSETS = ['BTC', 'ETH', 'SOL', 'XRP'] as const;
type OracleAsset = typeof ORACLE_ASSETS[number];

const OracleComparisonTab: React.FC = () => {
  const [asset, setAsset] = useState<OracleAsset>('BTC');
  const [data, setData] = useState<OracleComparisonResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchData = useCallback(async () => {
    try {
      const response = await api.getOracleComparison(asset.toLowerCase());
      setData(response);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to fetch');
    } finally {
      setLoading(false);
    }
  }, [asset]);

  useEffect(() => {
    fetchData();
    const interval = setInterval(fetchData, 1000); // 1Hz refresh
    return () => clearInterval(interval);
  }, [fetchData]);

  const assetData = data?.assets?.[asset];
  const rollingStats = assetData?.rolling_stats;
  const allTimeStats = assetData?.all_time_stats;
  const ticks = assetData?.price_ticks ?? [];
  const lastTick = ticks[ticks.length - 1];

  return (
    <div className="h-full flex flex-col">
      {/* Header with asset selector */}
      <div className="flex items-center justify-between border-b border-grey/40 pb-3 mb-3 flex-shrink-0">
        <div className="flex items-center gap-4">
          <h2 className="text-[14px] font-mono font-semibold text-fg">ORACLE COMPARISON</h2>
          <span className="text-[10px] font-mono text-fg/50">Chainlink vs Binance Settlement</span>
        </div>
        <div className="flex gap-1">
          {ORACLE_ASSETS.map(a => (
            <button
              key={a}
              onClick={() => setAsset(a)}
              className={[
                'px-3 py-1 text-[11px] font-mono border transition-colors',
                asset === a
                  ? 'bg-better-blue text-fg border-better-blue'
                  : 'border-grey/40 text-fg/90 hover:border-grey/60 hover:text-fg',
              ].join(' ')}
            >
              {a}
            </button>
          ))}
        </div>
      </div>

      {loading && !data && (
        <div className="flex-1 flex items-center justify-center text-fg/50 font-mono text-[12px]">Loading...</div>
      )}
      {error && !data && (
        <div className="flex-1 flex items-center justify-center text-danger font-mono text-[12px]">{error}</div>
      )}

      {data && (
        <div className="flex-1 grid grid-rows-[auto_1fr_1fr] gap-3 min-h-0">
          {/* Row 1: Stats Cards */}
          <div className="grid grid-cols-6 gap-2">
            <OracleStatCard label="ROLLING AGREE" value={rollingStats?.agreement_rate} format="pct" highlight />
            <OracleStatCard label="ALL-TIME AGREE" value={allTimeStats?.agreement_rate} format="pct" />
            <OracleStatCard label="DIVERGENCE" value={assetData?.current_divergence_bps} format="bps" />
            <OracleStatCard label="AVG DIV" value={rollingStats?.avg_divergence_bps} format="bps" />
            <OracleStatCard label="CL INTERVAL" value={assetData?.avg_chainlink_interval_us ? assetData.avg_chainlink_interval_us / 1000 : null} format="ms" />
            <OracleStatCard label="BN INTERVAL" value={assetData?.avg_binance_interval_us ? assetData.avg_binance_interval_us / 1000 : null} format="ms" />
          </div>

          {/* Row 2: Price Charts (left: Divergence, right: Comparison) */}
          <div className="grid grid-cols-2 gap-3 min-h-0">
            {/* Divergence Chart */}
            <div className="border border-grey/40 p-3 flex flex-col min-h-0">
              <div className="flex items-center justify-between mb-2 flex-shrink-0">
                <span className="text-[10px] font-mono text-fg/60">PRICE DIVERGENCE</span>
                <div className="flex items-baseline gap-2">
                  <span className={`text-[16px] font-mono font-semibold ${
                    lastTick?.divergence_bps !== null && lastTick?.divergence_bps !== undefined
                      ? Math.abs(lastTick.divergence_bps) < 10 ? 'text-green-400' : Math.abs(lastTick.divergence_bps) < 50 ? 'text-yellow-400' : 'text-red-400'
                      : 'text-fg'
                  }`}>
                    {lastTick?.divergence_bps !== null && lastTick?.divergence_bps !== undefined 
                      ? `${lastTick.divergence_bps > 0 ? '+' : ''}${lastTick.divergence_bps.toFixed(1)}bps` 
                      : '---'}
                  </span>
                </div>
              </div>
              <div className="flex-1 min-h-0">
                <OracleDivergenceChart ticks={ticks} />
              </div>
            </div>

            {/* Price Comparison Chart */}
            <div className="border border-grey/40 p-3 flex flex-col min-h-0">
              <div className="flex items-center justify-between mb-2 flex-shrink-0">
                <span className="text-[10px] font-mono text-fg/60">PRICE COMPARISON</span>
                <div className="flex items-baseline gap-3 text-[11px] font-mono">
                  <span className="text-blue-400">CL: ${lastTick?.chainlink_price?.toFixed(2) ?? '---'}</span>
                  <span className="text-orange-400">BN: ${lastTick?.binance_price?.toFixed(2) ?? '---'}</span>
                </div>
              </div>
              <div className="flex-1 min-h-0">
                <OraclePriceChart ticks={ticks} />
              </div>
            </div>
          </div>

          {/* Row 3: Resolution Table + Latency Chart */}
          <div className="grid grid-cols-2 gap-3 min-h-0">
            {/* Resolution History Table */}
            <div className="border border-grey/40 p-3 flex flex-col min-h-0">
              <div className="text-[10px] font-mono text-fg/60 mb-2 flex-shrink-0">
                15M RESOLUTION HISTORY ({assetData?.rolling_window?.length ?? 0} windows)
              </div>
              <div className="flex-1 min-h-0 overflow-auto">
                <WindowResolutionTable resolutions={assetData?.rolling_window ?? []} />
              </div>
            </div>

            {/* Latency Comparison */}
            <div className="border border-grey/40 p-3 flex flex-col min-h-0">
              <div className="text-[10px] font-mono text-fg/60 mb-2 flex-shrink-0">UPDATE LATENCY</div>
              <div className="flex-1 min-h-0">
                <OracleLatencyChart ticks={ticks} />
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
};

// Oracle stat card (matches EdgeCard from 15M tab)
const OracleStatCard: React.FC<{
  label: string;
  value: number | null | undefined;
  format: 'pct' | 'bps' | 'ms';
  highlight?: boolean;
}> = ({ label, value, format, highlight }) => {
  let displayValue = '---';
  let colorClass = 'text-fg';

  if (value !== null && value !== undefined) {
    if (format === 'pct') {
      displayValue = `${value.toFixed(1)}%`;
      colorClass = value >= 95 ? 'text-green-400' : value >= 80 ? 'text-yellow-400' : 'text-red-400';
    } else if (format === 'bps') {
      displayValue = `${value > 0 ? '+' : ''}${value.toFixed(1)}bps`;
      const absVal = Math.abs(value);
      colorClass = absVal < 10 ? 'text-green-400' : absVal < 50 ? 'text-yellow-400' : 'text-red-400';
    } else if (format === 'ms') {
      displayValue = `${value.toFixed(0)}ms`;
      colorClass = value < 100 ? 'text-green-400' : value < 500 ? 'text-yellow-400' : 'text-red-400';
    }
  }

  return (
    <div className={`border p-3 ${highlight ? 'border-better-blue/50 bg-better-blue/5' : 'border-grey/40'}`}>
      <div className="text-[9px] font-mono text-fg/60">{label}</div>
      <div className={`text-[18px] font-mono font-semibold ${colorClass}`}>{displayValue}</div>
    </div>
  );
};

// Divergence tick chart (shows bps divergence over time)
const OracleDivergenceChart: React.FC<{ ticks: OraclePriceTick[] }> = ({ ticks }) => {
  if (ticks.length < 2) {
    return (
      <div className="h-full flex items-center justify-center text-fg/50 text-[11px] font-mono">
        Collecting data...
      </div>
    );
  }

  const values = ticks.map(t => t.divergence_bps ?? 0);
  const max = Math.max(...values.map(Math.abs), 10); // Minimum ±10bps scale
  const min = -max;
  const range = max - min || 1;

  const width = 400;
  const height = 60;
  const padding = { left: 40, right: 10, top: 5, bottom: 5 };
  const chartWidth = width - padding.left - padding.right;
  const chartHeight = height - padding.top - padding.bottom;
  const xStep = chartWidth / (ticks.length - 1);
  const zeroY = padding.top + chartHeight / 2;

  const points = ticks.map((t, i) => {
    const x = padding.left + i * xStep;
    const y = padding.top + chartHeight - (((t.divergence_bps ?? 0) - min) / range) * chartHeight;
    return `${x},${y}`;
  }).join(' ');

  const current = ticks[ticks.length - 1]?.divergence_bps ?? 0;
  const color = Math.abs(current) < 10 ? '#22c55e' : Math.abs(current) < 50 ? '#eab308' : '#ef4444';

  return (
    <svg width="100%" height="100%" viewBox={`0 0 ${width} ${height}`} className="font-mono">
      {/* Zero line */}
      <line x1={padding.left} y1={zeroY} x2={width - padding.right} y2={zeroY} stroke="rgb(var(--c-fg) / 0.3)" strokeDasharray="2,2" />
      
      {/* Y axis labels */}
      <text x={padding.left - 5} y={padding.top + 5} textAnchor="end" fill="rgb(var(--c-fg) / 0.5)" fontSize="8">+{max.toFixed(0)}</text>
      <text x={padding.left - 5} y={zeroY + 3} textAnchor="end" fill="rgb(var(--c-fg) / 0.5)" fontSize="8">0</text>
      <text x={padding.left - 5} y={height - padding.bottom} textAnchor="end" fill="rgb(var(--c-fg) / 0.5)" fontSize="8">-{max.toFixed(0)}</text>

      {/* Divergence line */}
      <polyline points={points} fill="none" stroke={color} strokeWidth="1.5" />
      
      {/* Current value */}
      <text x={width - padding.right} y={padding.top + 12} textAnchor="end" fill={color} fontSize="10" fontWeight="bold">
        {current > 0 ? '+' : ''}{current.toFixed(1)}bps
      </text>
    </svg>
  );
};

// Window resolution table
const WindowResolutionTable: React.FC<{ resolutions: WindowResolution[] }> = ({ resolutions }) => {
  if (resolutions.length === 0) {
    return (
      <div className="h-full flex items-center justify-center text-fg/50 text-[11px] font-mono">
        No resolution data yet - waiting for 15m window completions
      </div>
    );
  }

  // Show most recent first
  const sorted = [...resolutions].sort((a, b) => b.window_end_ts - a.window_end_ts);

  return (
    <table className="w-full text-[10px] font-mono">
      <thead className="text-fg/60 sticky top-0 bg-void">
        <tr>
          <th className="text-left py-1 px-1">WINDOW END</th>
          <th className="text-right px-1">CL START</th>
          <th className="text-right px-1">CL END</th>
          <th className="text-center px-1">CL</th>
          <th className="text-right px-1">BN START</th>
          <th className="text-right px-1">BN END</th>
          <th className="text-center px-1">BN</th>
          <th className="text-center px-1">MATCH</th>
          <th className="text-right px-1">DIV (bps)</th>
        </tr>
      </thead>
      <tbody>
        {sorted.slice(0, 50).map((r, i) => {
          const clOutcome = r.chainlink_outcome === true ? 'UP' : r.chainlink_outcome === false ? 'DN' : '---';
          const bnOutcome = r.binance_outcome === true ? 'UP' : r.binance_outcome === false ? 'DN' : '---';
          const agreed = r.agreed === true ? 'YES' : r.agreed === false ? 'NO' : '---';
          const agreedColor = r.agreed === true ? 'text-green-400' : r.agreed === false ? 'text-red-400' : 'text-fg/50';
          const divColor = r.divergence_bps !== null 
            ? Math.abs(r.divergence_bps) < 10 ? 'text-green-400' : Math.abs(r.divergence_bps) < 50 ? 'text-yellow-400' : 'text-red-400'
            : 'text-fg/50';

          return (
            <tr key={i} className="border-b border-grey/10 hover:bg-grey/5">
              <td className="py-1 px-1 text-fg/80">{new Date(r.window_end_ts * 1000).toLocaleTimeString()}</td>
              <td className="py-1 px-1 text-right text-fg/60">{r.chainlink_start?.toFixed(2) ?? '---'}</td>
              <td className="py-1 px-1 text-right text-fg/80">{r.chainlink_end?.toFixed(2) ?? '---'}</td>
              <td className={`py-1 px-1 text-center ${r.chainlink_outcome === true ? 'text-green-400' : 'text-red-400'}`}>{clOutcome}</td>
              <td className="py-1 px-1 text-right text-fg/60">{r.binance_start?.toFixed(2) ?? '---'}</td>
              <td className="py-1 px-1 text-right text-fg/80">{r.binance_end?.toFixed(2) ?? '---'}</td>
              <td className={`py-1 px-1 text-center ${r.binance_outcome === true ? 'text-green-400' : 'text-red-400'}`}>{bnOutcome}</td>
              <td className={`py-1 px-1 text-center font-semibold ${agreedColor}`}>{agreed}</td>
              <td className={`py-1 px-1 text-right ${divColor}`}>
                {r.divergence_bps !== null ? `${r.divergence_bps > 0 ? '+' : ''}${r.divergence_bps.toFixed(1)}` : '---'}
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
};

// Price comparison chart (Chainlink vs Binance) - matches 15M PriceChart style
const OraclePriceChart: React.FC<{ ticks: OraclePriceTick[] }> = ({ ticks }) => {
  if (ticks.length < 2) {
    return (
      <div className="h-full flex items-center justify-center text-fg/50 text-[11px] font-mono">
        Collecting price data...
      </div>
    );
  }

  const clPrices = ticks.map(t => t.chainlink_price).filter((p): p is number => p !== null);
  const bnPrices = ticks.map(t => t.binance_price).filter((p): p is number => p !== null);
  
  if (clPrices.length < 2 && bnPrices.length < 2) {
    return (
      <div className="h-full flex items-center justify-center text-fg/50 text-[11px] font-mono">
        Waiting for price data...
      </div>
    );
  }

  const allPrices = [...clPrices, ...bnPrices];
  const min = Math.min(...allPrices);
  const max = Math.max(...allPrices);
  const range = max - min || 1;
  const paddedMin = min - range * 0.05;
  const paddedMax = max + range * 0.05;
  const paddedRange = paddedMax - paddedMin;

  const viewWidth = 100;
  const viewHeight = 100;
  const padding = { top: 2, right: 1, bottom: 2, left: 1 };
  const chartWidth = viewWidth - padding.left - padding.right;
  const chartHeight = viewHeight - padding.top - padding.bottom;

  const getPoints = (prices: (number | null)[]) => {
    const validPrices = prices.map((p, i) => ({ p, i })).filter(d => d.p !== null);
    if (validPrices.length < 2) return '';
    return validPrices.map(d => {
      const x = padding.left + (d.i / (prices.length - 1)) * chartWidth;
      const y = padding.top + chartHeight - ((d.p! - paddedMin) / paddedRange) * chartHeight;
      return `${x},${y}`;
    }).join(' ');
  };

  const clPoints = getPoints(ticks.map(t => t.chainlink_price));
  const bnPoints = getPoints(ticks.map(t => t.binance_price));

  return (
    <svg 
      viewBox={`0 0 ${viewWidth} ${viewHeight}`}
      preserveAspectRatio="none"
      className="w-full h-full"
    >
      {/* Binance line (orange) */}
      {bnPoints && (
        <polyline points={bnPoints} fill="none" stroke="#f97316" strokeWidth="1" vectorEffect="non-scaling-stroke" />
      )}
      {/* Chainlink line (blue) */}
      {clPoints && (
        <polyline points={clPoints} fill="none" stroke="#3b82f6" strokeWidth="1" vectorEffect="non-scaling-stroke" />
      )}
    </svg>
  );
};

// Latency comparison chart (shows update intervals over time)
const OracleLatencyChart: React.FC<{ ticks: OraclePriceTick[] }> = ({ ticks }) => {
  // Extract latency values (convert from microseconds to milliseconds)
  const clLatencies = ticks.map(t => t.chainlink_latency_us ? t.chainlink_latency_us / 1000 : null);
  const bnLatencies = ticks.map(t => t.binance_latency_us ? t.binance_latency_us / 1000 : null);
  
  const validCl = clLatencies.filter((l): l is number => l !== null);
  const validBn = bnLatencies.filter((l): l is number => l !== null);

  if (validCl.length < 2 && validBn.length < 2) {
    return (
      <div className="h-full flex items-center justify-center text-fg/50 text-[11px] font-mono">
        Collecting latency data...
      </div>
    );
  }

  const allLatencies = [...validCl, ...validBn].filter(l => l > 0);
  if (allLatencies.length === 0) {
    return (
      <div className="h-full flex items-center justify-center text-fg/50 text-[11px] font-mono">
        Waiting for updates...
      </div>
    );
  }

  const max = Math.max(...allLatencies, 100); // Minimum 100ms scale
  const min = 0;
  const range = max - min;

  const viewWidth = 100;
  const viewHeight = 100;
  const padding = { top: 2, right: 1, bottom: 2, left: 1 };
  const chartWidth = viewWidth - padding.left - padding.right;
  const chartHeight = viewHeight - padding.top - padding.bottom;

  const getPoints = (latencies: (number | null)[]) => {
    const validLatencies = latencies.map((l, i) => ({ l, i })).filter(d => d.l !== null && d.l > 0);
    if (validLatencies.length < 2) return '';
    return validLatencies.map(d => {
      const x = padding.left + (d.i / (latencies.length - 1)) * chartWidth;
      const y = padding.top + chartHeight - ((d.l! - min) / range) * chartHeight;
      return `${x},${y}`;
    }).join(' ');
  };

  const clPoints = getPoints(clLatencies);
  const bnPoints = getPoints(bnLatencies);
  
  const lastCl = validCl[validCl.length - 1];
  const lastBn = validBn[validBn.length - 1];

  return (
    <div className="h-full flex flex-col">
      <div className="flex-1 min-h-0">
        <svg 
          viewBox={`0 0 ${viewWidth} ${viewHeight}`}
          preserveAspectRatio="none"
          className="w-full h-full"
        >
          {/* Binance line (orange) */}
          {bnPoints && (
            <polyline points={bnPoints} fill="none" stroke="#f97316" strokeWidth="1" vectorEffect="non-scaling-stroke" />
          )}
          {/* Chainlink line (blue) */}
          {clPoints && (
            <polyline points={clPoints} fill="none" stroke="#3b82f6" strokeWidth="1" vectorEffect="non-scaling-stroke" />
          )}
        </svg>
      </div>
      {/* Legend */}
      <div className="flex-shrink-0 flex justify-between text-[9px] font-mono pt-1">
        <div className="flex gap-3">
          <span className="text-blue-400">CL: {lastCl?.toFixed(0) ?? '---'}ms</span>
          <span className="text-orange-400">BN: {lastBn?.toFixed(0) ?? '---'}ms</span>
        </div>
        <span className="text-fg/50">Update interval</span>
      </div>
    </div>
  );
};
