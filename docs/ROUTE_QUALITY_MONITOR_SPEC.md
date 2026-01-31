# Route Quality Monitor Specification

## Overview

Continuous monitoring system for detecting routing changes, packet loss, and latency drift on paths to Binance market data endpoints. Designed for Prometheus/Grafana with automatic mitigation.

## Target Endpoints

```yaml
binance_endpoints:
  websocket:
    primary: wss://stream.binance.com:9443
    backup:
      - wss://stream1.binance.com:9443
      - wss://stream2.binance.com:9443
      - wss://stream3.binance.com:9443
  rest:
    primary: https://api.binance.com
    backup:
      - https://api1.binance.com
      - https://api2.binance.com
      - https://api3.binance.com
  
  # IP-based endpoints (bypass DNS)
  direct_ips:
    # Resolve and cache these at startup; refresh on DNS TTL
    - endpoint: stream.binance.com
      port: 9443
      protocol: wss
```

---

## 1. Active Probing Methodology

### 1.1 Probe Types

| Probe Type | Frequency | Purpose | Method |
|------------|-----------|---------|--------|
| ICMP Ping | 1s | RTT baseline, packet loss | `ping -c 1 -W 1` |
| TCP Connect | 5s | Port reachability, TCP handshake time | `nc -zw1` or custom |
| TLS Handshake | 10s | Full TLS establishment time | Custom TLS probe |
| HTTP(S) Health | 30s | Application-layer health | `GET /api/v3/ping` |
| Traceroute | 60s | Path analysis, hop changes | `mtr --json` |
| DNS Resolution | 30s | DNS latency, IP changes | `dig +stats` |

### 1.2 Probe Implementation

```python
# Prometheus metrics exposed by the prober
route_quality_rtt_seconds{endpoint, probe_type, hop}
route_quality_packet_loss_ratio{endpoint}
route_quality_tcp_connect_seconds{endpoint, port}
route_quality_tls_handshake_seconds{endpoint}
route_quality_dns_resolution_seconds{endpoint}
route_quality_dns_ip_changed{endpoint}  # 1 if IP changed since last probe
route_quality_hop_count{endpoint}
route_quality_path_hash{endpoint}  # Hash of traceroute path for change detection
route_quality_probe_success{endpoint, probe_type}  # 1=success, 0=failure
```

### 1.3 Multi-Region Probing

If running from multiple regions, add region label:

```
route_quality_rtt_seconds{endpoint="stream.binance.com", region="us-east-1", probe_type="icmp"}
```

---

## 2. Thresholds

### 2.1 Latency Thresholds

| Metric | Warning | Critical | Action |
|--------|---------|----------|--------|
| ICMP RTT | > p99 + 2σ | > p99 + 4σ | Alert |
| ICMP RTT absolute | > 50ms | > 100ms | Alert + consider failover |
| TCP Connect | > 20ms | > 50ms | Alert |
| TLS Handshake | > 100ms | > 200ms | Alert + connection pool refresh |
| DNS Resolution | > 50ms | > 100ms | Switch to cached IP |

### 2.2 Packet Loss Thresholds

| Window | Warning | Critical | Action |
|--------|---------|----------|--------|
| 1 minute | > 0.1% | > 1% | Alert |
| 5 minute | > 0.05% | > 0.5% | Alert + failover consideration |
| Any | 3 consecutive | 5 consecutive | Immediate failover |

### 2.3 Stability Thresholds

| Metric | Threshold | Action |
|--------|-----------|--------|
| Path change (hop count delta) | > 2 hops | Alert |
| Path hash change | Any change | Alert + log new path |
| DNS IP change | Any change | Alert + connection refresh |
| RTT variance (1min) | > 3x baseline σ | Alert (jitter) |

### 2.4 Baseline Calculation

```yaml
baseline:
  window: 24h
  recalculate: every 1h
  metrics:
    - rtt_p50
    - rtt_p99
    - rtt_stddev
    - packet_loss_rate
    - typical_hop_count
  
  # Exclude anomalies from baseline
  outlier_removal: 3_sigma
```

---

## 3. Prometheus Recording Rules

```yaml
groups:
  - name: route_quality_baselines
    interval: 1m
    rules:
      # RTT baseline (24h rolling)
      - record: route_quality:rtt_p50:24h
        expr: |
          histogram_quantile(0.5,
            sum(rate(route_quality_rtt_seconds_bucket{probe_type="icmp"}[24h])) by (endpoint, le)
          )
      
      - record: route_quality:rtt_p99:24h
        expr: |
          histogram_quantile(0.99,
            sum(rate(route_quality_rtt_seconds_bucket{probe_type="icmp"}[24h])) by (endpoint, le)
          )
      
      - record: route_quality:rtt_stddev:1h
        expr: |
          stddev_over_time(route_quality_rtt_seconds{probe_type="icmp"}[1h])
      
      # Packet loss rate (5m window)
      - record: route_quality:packet_loss:5m
        expr: |
          1 - (
            sum(rate(route_quality_probe_success{probe_type="icmp"}[5m])) by (endpoint)
            /
            sum(rate(route_quality_probe_total{probe_type="icmp"}[5m])) by (endpoint)
          )
      
      # Path stability (change detection)
      - record: route_quality:path_changes:1h
        expr: |
          changes(route_quality_path_hash[1h])
      
      # DNS stability
      - record: route_quality:dns_changes:1h
        expr: |
          sum(increase(route_quality_dns_ip_changed[1h])) by (endpoint)

  - name: route_quality_scores
    interval: 30s
    rules:
      # Composite health score (0-100)
      - record: route_quality:health_score
        expr: |
          100 * (
            # RTT component (40% weight)
            0.4 * clamp_max(
              route_quality:rtt_p50:24h / route_quality_rtt_seconds{probe_type="icmp"}, 
              1
            )
            +
            # Packet loss component (40% weight)
            0.4 * (1 - clamp_max(route_quality:packet_loss:5m * 100, 1))
            +
            # Stability component (20% weight)
            0.2 * (1 - clamp_max(route_quality:path_changes:1h / 10, 1))
          )
```

---

## 4. Alert Rules

```yaml
groups:
  - name: route_quality_alerts
    rules:
      # === Latency Alerts ===
      
      - alert: BinanceRttWarning
        expr: |
          route_quality_rtt_seconds{probe_type="icmp"} 
          > route_quality:rtt_p99:24h + 2 * route_quality:rtt_stddev:1h
        for: 2m
        labels:
          severity: warning
          team: infrastructure
        annotations:
          summary: "Elevated RTT to {{ $labels.endpoint }}"
          description: "RTT {{ $value | humanizeDuration }} exceeds baseline by >2σ"
          runbook: "https://wiki/runbooks/route-quality#rtt-warning"
      
      - alert: BinanceRttCritical
        expr: |
          route_quality_rtt_seconds{probe_type="icmp"} > 0.1
          or
          route_quality_rtt_seconds{probe_type="icmp"} 
          > route_quality:rtt_p99:24h + 4 * route_quality:rtt_stddev:1h
        for: 1m
        labels:
          severity: critical
          team: infrastructure
        annotations:
          summary: "Critical RTT to {{ $labels.endpoint }}"
          description: "RTT {{ $value | humanizeDuration }} - consider failover"
          action: "failover_candidate"
      
      # === Packet Loss Alerts ===
      
      - alert: BinancePacketLossWarning
        expr: route_quality:packet_loss:5m > 0.001
        for: 2m
        labels:
          severity: warning
        annotations:
          summary: "Packet loss to {{ $labels.endpoint }}: {{ $value | humanizePercentage }}"
      
      - alert: BinancePacketLossCritical
        expr: route_quality:packet_loss:5m > 0.01
        for: 1m
        labels:
          severity: critical
        annotations:
          summary: "Critical packet loss to {{ $labels.endpoint }}"
          action: "immediate_failover"
      
      - alert: BinanceConsecutiveLoss
        expr: |
          sum_over_time(
            (route_quality_probe_success{probe_type="icmp"} == 0)[30s:1s]
          ) >= 5
        labels:
          severity: critical
        annotations:
          summary: "5+ consecutive probe failures to {{ $labels.endpoint }}"
          action: "immediate_failover"
      
      # === Path Change Alerts ===
      
      - alert: BinanceRouteChanged
        expr: changes(route_quality_path_hash[5m]) > 0
        labels:
          severity: warning
        annotations:
          summary: "Route to {{ $labels.endpoint }} changed"
          description: "Traceroute path modified - investigate for routing issues"
      
      - alert: BinanceHopCountAnomaly
        expr: |
          abs(route_quality_hop_count - avg_over_time(route_quality_hop_count[24h])) > 2
        for: 5m
        labels:
          severity: warning
        annotations:
          summary: "Hop count anomaly to {{ $labels.endpoint }}"
      
      # === DNS Alerts ===
      
      - alert: BinanceDnsChanged
        expr: route_quality_dns_ip_changed == 1
        labels:
          severity: info
        annotations:
          summary: "DNS IP changed for {{ $labels.endpoint }}"
          action: "connection_refresh"
      
      - alert: BinanceDnsSlow
        expr: route_quality_dns_resolution_seconds > 0.1
        for: 1m
        labels:
          severity: warning
        annotations:
          summary: "Slow DNS resolution for {{ $labels.endpoint }}"
          action: "use_cached_ip"
      
      # === Health Score ===
      
      - alert: BinanceHealthDegraded
        expr: route_quality:health_score < 80
        for: 2m
        labels:
          severity: warning
        annotations:
          summary: "Route health degraded: {{ $value }}%"
      
      - alert: BinanceHealthCritical
        expr: route_quality:health_score < 50
        for: 1m
        labels:
          severity: critical
        annotations:
          summary: "Route health critical: {{ $value }}%"
          action: "failover_required"
```

---

## 5. Automatic Mitigation Actions

### 5.1 Mitigation Controller

```yaml
mitigation_controller:
  # Receives alerts via Alertmanager webhook
  webhook_port: 9095
  
  actions:
    dns_refresh:
      trigger:
        - alert: BinanceDnsChanged
        - alert: BinanceDnsSlow
      cooldown: 60s
      action: |
        1. Clear DNS cache
        2. Re-resolve all Binance endpoints
        3. Update connection pool with new IPs
        4. Log IP mapping change
    
    connection_refresh:
      trigger:
        - alert: BinanceRttCritical
        - alert: BinanceTlsHandshakeSlow
      cooldown: 30s
      action: |
        1. Mark current connections as "draining"
        2. Establish new connections (parallel)
        3. Verify new connection health
        4. Switch traffic to new connections
        5. Close drained connections after timeout
    
    endpoint_failover:
      trigger:
        - alert: BinancePacketLossCritical
        - alert: BinanceConsecutiveLoss
        - alert: BinanceHealthCritical
      cooldown: 300s  # 5 min between failovers
      action: |
        1. Select next healthy endpoint from pool
        2. Verify candidate endpoint health (probe)
        3. If healthy: execute failover
        4. If all endpoints unhealthy: alert + exponential backoff retry
    
    path_change_investigation:
      trigger:
        - alert: BinanceRouteChanged
      cooldown: 0  # Always investigate
      action: |
        1. Log full traceroute diff
        2. Check if new path has better/worse metrics
        3. If worse: trigger connection_refresh
        4. Store path history for analysis
```

### 5.2 Failover State Machine

```
                    ┌─────────────────┐
                    │     HEALTHY     │
                    │  (primary EP)   │
                    └────────┬────────┘
                             │
                    health_score < 50
                    or consecutive_loss >= 5
                             │
                             ▼
                    ┌─────────────────┐
                    │   DEGRADED      │
                    │ (probe backups) │
                    └────────┬────────┘
                             │
                    backup_healthy?
                     /            \
                   yes             no
                   /                \
                  ▼                  ▼
        ┌─────────────────┐  ┌─────────────────┐
        │   FAILOVER      │  │   IMPAIRED      │
        │ (switch to      │  │ (retry primary  │
        │  backup EP)     │  │  with backoff)  │
        └────────┬────────┘  └────────┬────────┘
                 │                    │
        backup_stable?         primary_recovered?
                 │                    │
                 ▼                    ▼
        ┌─────────────────┐  ┌─────────────────┐
        │   HEALTHY       │  │   HEALTHY       │
        │  (backup EP)    │  │  (primary EP)   │
        └─────────────────┘  └─────────────────┘
```

### 5.3 DNS Refresh Policy

```yaml
dns_policy:
  # Normal operation
  refresh_interval: 300s  # 5 min
  respect_ttl: true
  min_ttl: 60s
  max_ttl: 3600s
  
  # On DNS change alert
  on_change:
    - clear_cache: true
    - re_resolve: all_endpoints
    - connection_refresh: true
  
  # On DNS failure
  on_failure:
    - use_cached_ip: true
    - retry_interval: 10s
    - max_retries: 6
    - fallback: hardcoded_ips  # Last resort
  
  # Caching
  cache:
    type: in_memory
    negative_cache_ttl: 30s
    
  # Multiple resolvers for redundancy
  resolvers:
    - 8.8.8.8        # Google
    - 1.1.1.1        # Cloudflare
    - system         # System resolver
  resolver_timeout: 2s
  resolver_strategy: first_success
```

### 5.4 Connection Re-establishment Policy

```yaml
connection_policy:
  # Pool configuration
  pool:
    min_connections: 2
    max_connections: 5
    idle_timeout: 300s
  
  # Health checks
  health_check:
    interval: 10s
    timeout: 5s
    unhealthy_threshold: 3
    healthy_threshold: 2
  
  # Re-establishment triggers
  refresh_triggers:
    - rtt_exceeds_baseline_4sigma
    - tls_handshake_slow
    - dns_ip_changed
    - connection_age > 3600s  # Hourly refresh
  
  # Refresh procedure
  refresh:
    strategy: rolling  # or "all_at_once"
    new_connection_verify: true
    drain_timeout: 30s
    parallel_establish: 2
  
  # Backoff on failure
  backoff:
    initial: 1s
    max: 60s
    multiplier: 2
    jitter: 0.1
```

### 5.5 Multi-Endpoint Failover Policy

```yaml
failover_policy:
  # Endpoint priority
  endpoints:
    - url: wss://stream.binance.com:9443
      priority: 1
      weight: 100
    - url: wss://stream1.binance.com:9443
      priority: 2
      weight: 50
    - url: wss://stream2.binance.com:9443
      priority: 2
      weight: 50
    - url: wss://stream3.binance.com:9443
      priority: 3
      weight: 25
  
  # Selection strategy
  selection:
    strategy: priority_then_health
    health_weight: 0.6
    latency_weight: 0.4
  
  # Failover triggers
  triggers:
    - health_score < 50
    - consecutive_failures >= 5
    - packet_loss > 1%
  
  # Failover procedure
  procedure:
    verify_candidate: true
    verify_timeout: 5s
    switch_traffic: atomic  # vs gradual
    rollback_on_failure: true
  
  # Failback policy
  failback:
    enabled: true
    primary_recovery_check_interval: 60s
    primary_stable_duration: 300s  # 5 min stable before failback
    gradual_traffic_shift: true
  
  # Circuit breaker
  circuit_breaker:
    enabled: true
    failure_threshold: 5
    success_threshold: 3
    timeout: 60s
```

---

## 6. Grafana Dashboard

### 6.1 Dashboard Panels

```json
{
  "title": "Binance Route Quality",
  "panels": [
    {
      "title": "RTT to Binance Endpoints",
      "type": "timeseries",
      "targets": [
        {
          "expr": "route_quality_rtt_seconds{probe_type=\"icmp\"} * 1000",
          "legendFormat": "{{ endpoint }}"
        },
        {
          "expr": "route_quality:rtt_p99:24h * 1000",
          "legendFormat": "p99 baseline"
        }
      ],
      "fieldConfig": {
        "defaults": {
          "unit": "ms",
          "thresholds": {
            "steps": [
              {"value": 0, "color": "green"},
              {"value": 50, "color": "yellow"},
              {"value": 100, "color": "red"}
            ]
          }
        }
      }
    },
    {
      "title": "Packet Loss Rate",
      "type": "stat",
      "targets": [
        {
          "expr": "route_quality:packet_loss:5m * 100",
          "legendFormat": "{{ endpoint }}"
        }
      ],
      "fieldConfig": {
        "defaults": {
          "unit": "percent",
          "thresholds": {
            "steps": [
              {"value": 0, "color": "green"},
              {"value": 0.1, "color": "yellow"},
              {"value": 1, "color": "red"}
            ]
          }
        }
      }
    },
    {
      "title": "Health Score",
      "type": "gauge",
      "targets": [
        {
          "expr": "route_quality:health_score",
          "legendFormat": "{{ endpoint }}"
        }
      ],
      "fieldConfig": {
        "defaults": {
          "min": 0,
          "max": 100,
          "thresholds": {
            "steps": [
              {"value": 0, "color": "red"},
              {"value": 50, "color": "yellow"},
              {"value": 80, "color": "green"}
            ]
          }
        }
      }
    },
    {
      "title": "Route Path Visualization",
      "type": "nodeGraph",
      "description": "Traceroute hops to Binance"
    },
    {
      "title": "Active Endpoint",
      "type": "stat",
      "targets": [
        {
          "expr": "binance_active_endpoint_info",
          "legendFormat": "{{ endpoint }}"
        }
      ]
    },
    {
      "title": "Failover Events",
      "type": "timeseries",
      "targets": [
        {
          "expr": "increase(binance_failover_total[1h])",
          "legendFormat": "Failovers"
        }
      ]
    },
    {
      "title": "DNS Resolution Time",
      "type": "timeseries",
      "targets": [
        {
          "expr": "route_quality_dns_resolution_seconds * 1000",
          "legendFormat": "{{ endpoint }}"
        }
      ]
    },
    {
      "title": "Path Changes (24h)",
      "type": "stat",
      "targets": [
        {
          "expr": "sum(route_quality:path_changes:1h)",
          "legendFormat": "Total"
        }
      ]
    }
  ]
}
```

### 6.2 Dashboard Variables

```yaml
variables:
  - name: endpoint
    type: query
    query: label_values(route_quality_rtt_seconds, endpoint)
    multi: true
    includeAll: true
  
  - name: region
    type: query
    query: label_values(route_quality_rtt_seconds, region)
    multi: true
```

---

## 7. Prober Implementation (Rust)

### 7.1 Core Metrics

```rust
// src/route_quality/metrics.rs

use prometheus::{Histogram, HistogramVec, IntCounterVec, IntGaugeVec, Registry};

pub struct RouteQualityMetrics {
    pub rtt_seconds: HistogramVec,
    pub packet_loss_ratio: GaugeVec,
    pub tcp_connect_seconds: HistogramVec,
    pub tls_handshake_seconds: HistogramVec,
    pub dns_resolution_seconds: HistogramVec,
    pub dns_ip_changed: IntGaugeVec,
    pub hop_count: IntGaugeVec,
    pub path_hash: IntGaugeVec,
    pub probe_success: IntCounterVec,
    pub probe_total: IntCounterVec,
    pub health_score: GaugeVec,
    pub failover_total: IntCounterVec,
    pub active_endpoint: IntGaugeVec,
}

impl RouteQualityMetrics {
    pub fn new(registry: &Registry) -> Self {
        let rtt_buckets = vec![
            0.001, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0
        ];
        
        Self {
            rtt_seconds: HistogramVec::new(
                HistogramOpts::new("route_quality_rtt_seconds", "RTT in seconds")
                    .buckets(rtt_buckets.clone()),
                &["endpoint", "probe_type"]
            ).unwrap(),
            // ... other metrics
        }
    }
}
```

### 7.2 Prober Loop

```rust
// src/route_quality/prober.rs

pub struct RouteQualityProber {
    endpoints: Vec<Endpoint>,
    metrics: Arc<RouteQualityMetrics>,
    mitigation_tx: mpsc::Sender<MitigationAction>,
}

impl RouteQualityProber {
    pub async fn run(&self) {
        let mut icmp_interval = interval(Duration::from_secs(1));
        let mut tcp_interval = interval(Duration::from_secs(5));
        let mut tls_interval = interval(Duration::from_secs(10));
        let mut dns_interval = interval(Duration::from_secs(30));
        let mut traceroute_interval = interval(Duration::from_secs(60));
        
        loop {
            tokio::select! {
                _ = icmp_interval.tick() => {
                    self.probe_icmp().await;
                }
                _ = tcp_interval.tick() => {
                    self.probe_tcp().await;
                }
                _ = tls_interval.tick() => {
                    self.probe_tls().await;
                }
                _ = dns_interval.tick() => {
                    self.probe_dns().await;
                }
                _ = traceroute_interval.tick() => {
                    self.probe_traceroute().await;
                }
            }
        }
    }
    
    async fn probe_icmp(&self) {
        for endpoint in &self.endpoints {
            let start = Instant::now();
            match ping(endpoint.ip, Duration::from_secs(1)).await {
                Ok(rtt) => {
                    self.metrics.rtt_seconds
                        .with_label_values(&[&endpoint.name, "icmp"])
                        .observe(rtt.as_secs_f64());
                    self.metrics.probe_success
                        .with_label_values(&[&endpoint.name, "icmp"])
                        .inc();
                }
                Err(_) => {
                    // Record failure, check for consecutive failures
                    self.handle_probe_failure(endpoint, "icmp").await;
                }
            }
            self.metrics.probe_total
                .with_label_values(&[&endpoint.name, "icmp"])
                .inc();
        }
    }
    
    async fn handle_probe_failure(&self, endpoint: &Endpoint, probe_type: &str) {
        let failures = self.consecutive_failures.fetch_add(1, Ordering::SeqCst);
        if failures >= 5 {
            self.mitigation_tx.send(MitigationAction::Failover {
                from: endpoint.clone(),
                reason: "consecutive_failures".into(),
            }).await.ok();
        }
    }
}
```

---

## 8. Alertmanager Configuration

```yaml
# alertmanager.yml

global:
  resolve_timeout: 5m

route:
  receiver: 'default'
  group_by: ['alertname', 'endpoint']
  group_wait: 10s
  group_interval: 5m
  repeat_interval: 4h
  
  routes:
    # Critical alerts: immediate action
    - match:
        severity: critical
      receiver: 'pagerduty-critical'
      group_wait: 0s
      repeat_interval: 5m
      routes:
        # Failover actions go to mitigation controller
        - match:
            action: immediate_failover
          receiver: 'mitigation-controller'
          continue: true
        - match:
            action: failover_required
          receiver: 'mitigation-controller'
          continue: true
    
    # DNS changes: trigger connection refresh
    - match:
        action: connection_refresh
      receiver: 'mitigation-controller'
    
    # Warning alerts
    - match:
        severity: warning
      receiver: 'slack-warnings'

receivers:
  - name: 'default'
    slack_configs:
      - channel: '#alerts-infra'
  
  - name: 'pagerduty-critical'
    pagerduty_configs:
      - service_key: '<key>'
  
  - name: 'slack-warnings'
    slack_configs:
      - channel: '#alerts-infra'
        send_resolved: true
  
  - name: 'mitigation-controller'
    webhook_configs:
      - url: 'http://localhost:9095/alert'
        send_resolved: true

inhibit_rules:
  - source_match:
      severity: 'critical'
    target_match:
      severity: 'warning'
    equal: ['endpoint']
```

---

## 9. Deployment

### 9.1 Docker Compose

```yaml
version: '3.8'

services:
  route-quality-prober:
    build: ./prober
    environment:
      - ENDPOINTS=stream.binance.com,api.binance.com
      - PROMETHEUS_PORT=9090
      - MITIGATION_WEBHOOK=http://mitigation-controller:9095
    cap_add:
      - NET_RAW  # For ICMP ping
    networks:
      - monitoring

  prometheus:
    image: prom/prometheus:latest
    volumes:
      - ./prometheus.yml:/etc/prometheus/prometheus.yml
      - ./rules:/etc/prometheus/rules
    ports:
      - "9090:9090"
    networks:
      - monitoring

  alertmanager:
    image: prom/alertmanager:latest
    volumes:
      - ./alertmanager.yml:/etc/alertmanager/alertmanager.yml
    ports:
      - "9093:9093"
    networks:
      - monitoring

  grafana:
    image: grafana/grafana:latest
    volumes:
      - ./dashboards:/var/lib/grafana/dashboards
    ports:
      - "3001:3000"
    networks:
      - monitoring

  mitigation-controller:
    build: ./mitigation
    environment:
      - WEBHOOK_PORT=9095
      - APP_CONTROL_ENDPOINT=http://betterbot:3000/internal/route-control
    networks:
      - monitoring

networks:
  monitoring:
    driver: bridge
```

### 9.2 Prometheus Scrape Config

```yaml
# prometheus.yml

global:
  scrape_interval: 15s
  evaluation_interval: 15s

rule_files:
  - /etc/prometheus/rules/*.yml

alerting:
  alertmanagers:
    - static_configs:
        - targets: ['alertmanager:9093']

scrape_configs:
  - job_name: 'route-quality-prober'
    scrape_interval: 5s
    static_configs:
      - targets: ['route-quality-prober:9090']
  
  - job_name: 'betterbot'
    static_configs:
      - targets: ['betterbot:3000']
```

---

## 10. Integration with BetterBot

### 10.1 Route Control API

Add internal endpoint to BetterBot for mitigation controller:

```rust
// POST /internal/route-control
#[derive(Deserialize)]
pub struct RouteControlRequest {
    pub action: RouteAction,
    pub endpoint: Option<String>,
    pub reason: String,
}

#[derive(Deserialize)]
pub enum RouteAction {
    DnsRefresh,
    ConnectionRefresh,
    Failover { to: String },
    Failback,
}
```

### 10.2 Health Export

Export current connection state for Prometheus:

```rust
// Expose as metrics
binance_ws_connection_state{endpoint} = 1  // 1=connected, 0=disconnected
binance_ws_connection_age_seconds{endpoint}
binance_active_endpoint_info{endpoint, ip, port} = 1
```
