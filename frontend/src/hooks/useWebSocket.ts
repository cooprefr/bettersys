import { useEffect, useState, useCallback, useRef } from 'react';
import { wsClient, WebSocketStatus } from '../services/websocket';
import { Signal, SignalContextUpdate } from '../types/signal';
import { useSignalStore } from '../stores/signalStore';

export const useWebSocket = () => {
  const [status, setStatus] = useState<WebSocketStatus>('disconnected');
  const [latency, setLatency] = useState<number>(0);
  const addSignals = useSignalStore((state) => state.addSignals);
  const applySignalContextUpdate = useSignalStore((state) => state.applySignalContextUpdate);

  const signalBufferRef = useRef<Signal[]>([]);
  const flushTimerRef = useRef<number | null>(null);

  useEffect(() => {
    // Connect to WebSocket
    wsClient.connect();

    // Listen for status changes
    const handleStatus = (newStatus: WebSocketStatus) => {
      setStatus(newStatus);
    };

    // Listen for new signals
    const handleSignal = (signal: Signal) => {
      // Buffer signals to avoid UI thrash (WS replay can send 100s of signals instantly).
      signalBufferRef.current.push(signal);
      if (flushTimerRef.current == null) {
        flushTimerRef.current = window.setTimeout(() => {
          const batch = signalBufferRef.current;
          signalBufferRef.current = [];
          flushTimerRef.current = null;
          addSignals(batch);
        }, 50);
      }
      
      // Play sound for high-confidence signals
      if (signal.confidence >= 0.90) {
        playNotificationSound();
      }
    };

    // Listen for signal enrichment context updates
    const handleSignalContext = (update: SignalContextUpdate) => {
      applySignalContextUpdate(update);
    };

    // Listen for pong (latency measurement) - high precision
    const handlePong = (data: any) => {
      if (data.timestamp) {
        // Use performance.now() for sub-millisecond precision
        const roundTripTime = performance.now() - data.timestamp;
        setLatency(roundTripTime);
      }
    };

    wsClient.on('status', handleStatus);
    wsClient.on('signal', handleSignal);
    wsClient.on('signal_context', handleSignalContext);
    wsClient.on('pong', handlePong);

    // Cleanup on unmount
    return () => {
      if (flushTimerRef.current != null) {
        window.clearTimeout(flushTimerRef.current);
        flushTimerRef.current = null;
      }
      wsClient.off('status', handleStatus);
      wsClient.off('signal', handleSignal);
      wsClient.off('signal_context', handleSignalContext);
      wsClient.off('pong', handlePong);
      wsClient.disconnect();
    };
  }, [addSignals, applySignalContextUpdate]);

  const reconnect = useCallback(() => {
    wsClient.disconnect();
    wsClient.connect();
  }, []);

  return {
    status,
    latency,
    reconnect,
    isConnected: status === 'connected',
  };
};

// Simple notification sound
function playNotificationSound() {
  try {
    const audioContext = new (window.AudioContext || (window as any).webkitAudioContext)();
    const oscillator = audioContext.createOscillator();
    const gainNode = audioContext.createGain();

    oscillator.connect(gainNode);
    gainNode.connect(audioContext.destination);

    oscillator.frequency.value = 800;
    oscillator.type = 'sine';
    
    gainNode.gain.setValueAtTime(0.3, audioContext.currentTime);
    gainNode.gain.exponentialRampToValueAtTime(0.01, audioContext.currentTime + 0.3);

    oscillator.start(audioContext.currentTime);
    oscillator.stop(audioContext.currentTime + 0.3);
  } catch (error) {
    console.warn('Failed to play notification sound:', error);
  }
}
