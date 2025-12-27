import { create } from 'zustand';
import { Signal, SignalContextUpdate, SignalStats } from '../types/signal';

const DEBUG = import.meta.env.DEV;

const HISTORY_WINDOW_MS = 24 * 60 * 60 * 1000;
const MAX_SIGNALS = 20_000;

interface SignalStore {
  signals: Signal[];
  stats: SignalStats | null;
  isLoading: boolean;
  error: string | null;
  
  addSignal: (signal: Signal) => void;
  addSignals: (signals: Signal[]) => void;
  applySignalContextUpdate: (update: SignalContextUpdate) => void;
  setSignals: (signals: Signal[]) => void;
  setStats: (stats: SignalStats) => void;
  clearSignals: () => void;
  setError: (error: string | null) => void;
}

function mergeSignal(existing: Signal, incoming: Signal): Signal {
  const existingCv = typeof existing.context_version === 'number' ? existing.context_version : -1;
  const incomingCv = typeof incoming.context_version === 'number' ? incoming.context_version : -1;

  const preferExistingCtx = existingCv > incomingCv;

  const context = preferExistingCtx ? existing.context : incoming.context ?? existing.context;
  const context_status = preferExistingCtx
    ? existing.context_status
    : incoming.context_status ?? existing.context_status;
  const context_version = preferExistingCtx
    ? existing.context_version
    : incoming.context_version ?? existing.context_version;
  const context_enriched_at = preferExistingCtx
    ? existing.context_enriched_at
    : incoming.context_enriched_at ?? existing.context_enriched_at;

  return {
    ...existing,
    ...incoming,
    context,
    context_status,
    context_version,
    context_enriched_at,
  };
}

function sortSignalsNewestFirst(signals: Signal[]): Signal[] {
  return signals.sort((a, b) => {
    const ta = Date.parse(a.detected_at);
    const tb = Date.parse(b.detected_at);

    const da = Number.isFinite(ta) ? ta : 0;
    const db = Number.isFinite(tb) ? tb : 0;

    const diff = db - da;
    if (diff !== 0) return diff;

    return a.id.localeCompare(b.id);
  });
}

function trimSignalsToWindow(signals: Signal[]): Signal[] {
  const cutoff = Date.now() - HISTORY_WINDOW_MS;
  const filtered = signals.filter((s) => {
    const t = Date.parse(s.detected_at);
    return Number.isNaN(t) || t >= cutoff;
  });

  return filtered.slice(0, MAX_SIGNALS);
}

export const useSignalStore = create<SignalStore>((set) => ({
  signals: [],
  stats: null,
  isLoading: false,
  error: null,

  addSignal: (signal) =>
    set((state) => {
      const existing = state.signals.find((s) => s.id === signal.id);
      const merged: Signal = existing ? mergeSignal(existing, signal) : signal;

      const next = [merged, ...state.signals.filter((s) => s.id !== signal.id)];
      return { signals: trimSignalsToWindow(sortSignalsNewestFirst(next)) };
    }),

  addSignals: (signals) =>
    set((state) => {
      if (!signals || signals.length === 0) return {};

      const byId = new Map<string, Signal>(state.signals.map((s) => [s.id, s]));
      for (const s of signals) {
        const existing = byId.get(s.id);
        byId.set(s.id, existing ? mergeSignal(existing, s) : s);
      }

      const merged = trimSignalsToWindow(sortSignalsNewestFirst(Array.from(byId.values())));
      return { signals: merged };
    }),

  applySignalContextUpdate: (update) =>
    set((state) => {
      const matchingSignal = state.signals.find((s) => s.id === update.signal_id);
      if (DEBUG) {
        if (matchingSignal) {
          console.log('Applying context to signal:', update.signal_id, 'status:', update.status);
        } else {
          console.log('No matching signal for context:', update.signal_id);
        }
      }
      return {
        signals: state.signals.map((s) =>
          s.id === update.signal_id
            ? {
                ...s,
                context: update.context,
                context_status: update.status,
                context_version: update.context_version,
                context_enriched_at: update.enriched_at,
              }
            : s
        ),
      };
    }),

  setSignals: (signals) =>
    set((state) => {
      // Merge fetched signals into the existing list.
      // IMPORTANT: WebSocket replay sends signals without `context`; the REST response contains
      // `context` and must be able to hydrate existing signals (same ids).
      const byId = new Map<string, Signal>(state.signals.map((s) => [s.id, s]));
      for (const s of signals) {
        const existing = byId.get(s.id);
        byId.set(s.id, existing ? mergeSignal(existing, s) : s);
      }

      const merged = trimSignalsToWindow(sortSignalsNewestFirst(Array.from(byId.values())));
      return { signals: merged, isLoading: false, error: null };
    }),

  setStats: (stats) =>
    set({ stats }),

  clearSignals: () =>
    set({ signals: [], stats: null, error: null }),

  setError: (error) =>
    set({ error, isLoading: false }),
}));
