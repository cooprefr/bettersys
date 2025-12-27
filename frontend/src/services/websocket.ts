// WebSocket Client for real-time signal feed

import { Signal } from '../types/signal';

const WS_URL = import.meta.env.VITE_WS_URL || 'ws://localhost:3000/ws';
const DEBUG = import.meta.env.DEV;
const PING_INTERVAL_MS = Number(import.meta.env.VITE_WS_PING_MS) || 100;

export type WebSocketStatus = 'connecting' | 'connected' | 'disconnected' | 'error';

export interface WebSocketMessage {
  type: 'signal' | 'signal_context' | 'ping' | 'pong' | 'stats';
  data: any;
}

export class WebSocketClient {
  private ws: WebSocket | null = null;
  private reconnectAttempts = 0;
  private maxReconnectAttempts = 5;
  private reconnectDelay = 1000;
  private pingInterval: number | null = null;
  private listeners: Map<string, Set<(data: any) => void>> = new Map();
  private lastPingTimestamp = 0;

  connect() {
    try {
      // Get token from localStorage (same key as api.ts)
      const token = localStorage.getItem('betterbot_token');
      
      // Append token as query parameter for backend auth middleware
      // Standard WebSocket doesn't support custom headers
      const wsUrlWithToken = token ? `${WS_URL}?token=${token}` : WS_URL;
      
      this.ws = new WebSocket(wsUrlWithToken);

      this.ws.onopen = () => {
        if (DEBUG) console.log('WebSocket connected');
        this.reconnectAttempts = 0;
        this.emit('status', 'connected');
        this.startPing();
      };

      this.ws.onmessage = (event) => {
        try {
          const message: WebSocketMessage = JSON.parse(event.data);
          this.handleMessage(message);
        } catch (error) {
          console.error('Failed to parse WebSocket message:', error);
        }
      };

      this.ws.onerror = (error) => {
        console.error('WebSocket error:', error);
        this.emit('status', 'error');
      };

      this.ws.onclose = () => {
        if (DEBUG) console.log('WebSocket disconnected');
        this.emit('status', 'disconnected');
        this.stopPing();
        this.attemptReconnect();
      };
    } catch (error) {
      console.error('Failed to create WebSocket:', error);
      this.emit('status', 'error');
    }
  }

  disconnect() {
    this.stopPing();
    if (this.ws) {
      this.ws.close();
      this.ws = null;
    }
  }

  send(message: WebSocketMessage) {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify(message));
    }
  }

  on(event: string, callback: (data: any) => void) {
    if (!this.listeners.has(event)) {
      this.listeners.set(event, new Set());
    }
    this.listeners.get(event)!.add(callback);
  }

  off(event: string, callback: (data: any) => void) {
    if (this.listeners.has(event)) {
      this.listeners.get(event)!.delete(callback);
    }
  }

  private emit(event: string, data: any) {
    if (this.listeners.has(event)) {
      this.listeners.get(event)!.forEach((callback) => callback(data));
    }
  }

  private handleMessage(message: WebSocketMessage) {
    switch (message.type) {
      case 'signal':
        this.emit('signal', message.data as Signal);
        break;
      case 'signal_context':
        if (DEBUG) {
          console.log('Received signal_context:', message.data?.signal_id, message.data?.status);
        }
        this.emit('signal_context', message.data);
        break;
      case 'pong':
        this.emit('pong', message.data);
        break;
      case 'stats':
        this.emit('stats', message.data);
        break;
      default:
        console.warn('Unknown message type:', message.type);
    }
  }

  private startPing() {
    // Send initial ping immediately to get latency
    this.lastPingTimestamp = performance.now();
    this.send({ type: 'ping', data: { timestamp: this.lastPingTimestamp } });
    
    // Ping periodically for latency monitoring (keep this modest to avoid extra load)
    this.pingInterval = setInterval(() => {
      this.lastPingTimestamp = performance.now();
      this.send({ type: 'ping', data: { timestamp: this.lastPingTimestamp } });
    }, PING_INTERVAL_MS);
  }

  private stopPing() {
    if (this.pingInterval) {
      clearInterval(this.pingInterval);
      this.pingInterval = null;
    }
  }

  private attemptReconnect() {
    if (this.reconnectAttempts < this.maxReconnectAttempts) {
      this.reconnectAttempts++;
      const delay = this.reconnectDelay * this.reconnectAttempts;
      if (DEBUG) {
        console.log(
          `Attempting to reconnect in ${delay}ms (${this.reconnectAttempts}/${this.maxReconnectAttempts})`
        );
      }
      
      setTimeout(() => {
        this.connect();
      }, delay);
    } else {
      console.error('Max reconnection attempts reached');
      this.emit('status', 'error');
    }
  }
}

export const wsClient = new WebSocketClient();
