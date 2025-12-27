# Phase 7 Implementation Plan: Authentication & API Security

**Target:** Production-Grade Security Infrastructure  
**Duration:** 4-6 hours  
**Status:** Planning → Implementation

---

## Executive Summary

Phase 7 transforms BetterBot from a functional system to a **production-secure system** with:
1. JWT-based authentication
2. API key management
3. Rate limiting & DDoS protection
4. Role-based access control (RBAC)
5. Secure WebSocket connections
6. Audit logging

**Security is not optional. It's mandatory for production.**

---

## Security Requirements

### Current State (Phase 6)
- ❌ No authentication
- ❌ Open API endpoints
- ❌ Unlimited request rates
- ❌ No access control
- ❌ Insecure WebSockets
- ⚠️ **Anyone can access everything**

### Target State (Phase 7)
- ✅ JWT authentication
- ✅ Protected API endpoints
- ✅ Rate limiting (per user/IP)
- ✅ Role-based permissions
- ✅ Authenticated WebSockets
- ✅ **Only authorized users with proper permissions**

---

## Part A: JWT Authentication System

### Architecture

```
Client → Login (username/password) → Server validates → JWT token issued
Client → API Request + JWT token → Server validates token → Access granted/denied
```

### Implementation

#### 1. Dependencies (Cargo.toml)
```toml
[dependencies]
jsonwebtoken = "9.2"
bcrypt = "0.15"
uuid = { version = "1.6", features = ["v4", "serde"] }
```

#### 2. Auth Models (`rust-backend/src/auth/models.rs`)
```rust
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub password_hash: String, // bcrypt hash
    pub role: UserRole,
    pub api_key: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum UserRole {
    Admin,      // Full access
    Trader,     // Signal access + trading
    Viewer,     // Read-only access
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,        // user_id
    pub username: String,
    pub role: UserRole,
    pub exp: usize,         // expiration timestamp
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub expires_in: usize,  // seconds
}
```

#### 3. JWT Handler (`rust-backend/src/auth/jwt.rs`)
```rust
use jsonwebtoken::{encode, decode, Header, Validation, EncodingKey, DecodingKey};
use anyhow::{Result, Context};

pub struct JwtHandler {
    secret: String,
    expiration_hours: usize,
}

impl JwtHandler {
    pub fn new(secret: String) -> Self {
        Self {
            secret,
            expiration_hours: 24, // 24-hour tokens
        }
    }

    pub fn generate_token(&self, user: &User) -> Result<String> {
        let expiration = chrono::Utc::now()
            .checked_add_signed(chrono::Duration::hours(self.expiration_hours as i64))
            .context("Invalid timestamp")?
            .timestamp() as usize;

        let claims = Claims {
            sub: user.id.to_string(),
            username: user.username.clone(),
            role: user.role.clone(),
            exp: expiration,
        };

        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(self.secret.as_bytes()),
        )
        .context("Failed to generate JWT")
    }

    pub fn validate_token(&self, token: &str) -> Result<Claims> {
        decode::<Claims>(
            token,
            &DecodingKey::from_secret(self.secret.as_bytes()),
            &Validation::default(),
        )
        .map(|data| data.claims)
        .context("Invalid or expired token")
    }
}
```

#### 4. User Storage (`rust-backend/src/auth/user_store.rs`)
```rust
use rusqlite::{Connection, params};
use bcrypt::{hash, verify, DEFAULT_COST};

pub struct UserStore {
    db_path: String,
}

impl UserStore {
    pub fn new(db_path: &str) -> Result<Self> {
        let store = Self {
            db_path: db_path.to_string(),
        };
        store.init_db()?;
        Ok(store)
    }

    fn init_db(&self) -> Result<()> {
        let conn = Connection::open(&self.db_path)?;
        
        conn.execute(
            "CREATE TABLE IF NOT EXISTS users (
                id TEXT PRIMARY KEY,
                username TEXT UNIQUE NOT NULL,
                password_hash TEXT NOT NULL,
                role TEXT NOT NULL,
                api_key TEXT,
                created_at TEXT NOT NULL
            )",
            [],
        )?;

        // Create default admin user if not exists
        self.create_default_admin(&conn)?;
        
        Ok(())
    }

    fn create_default_admin(&self, conn: &Connection) -> Result<()> {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM users WHERE role = 'Admin'",
            [],
            |row| row.get(0),
        )?;

        if count == 0 {
            let admin = User {
                id: Uuid::new_v4(),
                username: "admin".to_string(),
                password_hash: hash("admin123", DEFAULT_COST)?,
                role: UserRole::Admin,
                api_key: None,
                created_at: Utc::now().to_rfc3339(),
            };
            self.insert_user(&admin)?;
            info!("🔐 Default admin user created (username: admin, password: admin123)");
            warn!("⚠️  CHANGE DEFAULT PASSWORD IN PRODUCTION!");
        }

        Ok(())
    }

    pub fn get_user_by_username(&self, username: &str) -> Result<Option<User>> {
        let conn = Connection::open(&self.db_path)?;
        
        let mut stmt = conn.prepare(
            "SELECT id, username, password_hash, role, api_key, created_at 
             FROM users WHERE username = ?1"
        )?;

        let user = stmt.query_row(params![username], |row| {
            Ok(User {
                id: Uuid::parse_str(&row.get::<_, String>(0)?).unwrap(),
                username: row.get(1)?,
                password_hash: row.get(2)?,
                role: serde_json::from_str(&row.get::<_, String>(3)?).unwrap(),
                api_key: row.get(4)?,
                created_at: row.get(5)?,
            })
        }).optional()?;

        Ok(user)
    }

    pub fn verify_password(&self, username: &str, password: &str) -> Result<bool> {
        match self.get_user_by_username(username)? {
            Some(user) => Ok(verify(password, &user.password_hash)?),
            None => Ok(false),
        }
    }
}
```

---

## Part B: API Key Management

### Use Cases
1. Programmatic access (no username/password)
2. Integration with external systems
3. Revocable access tokens
4. Per-key rate limits

### Implementation

```rust
pub struct ApiKey {
    pub key: String,           // "btb_live_xxxxxxxxxxxx"
    pub user_id: Uuid,
    pub name: String,          // "Production Bot #1"
    pub permissions: Vec<String>,
    pub rate_limit: usize,     // requests per minute
    pub created_at: String,
    pub last_used: Option<String>,
    pub revoked: bool,
}

impl ApiKey {
    pub fn generate() -> String {
        format!("btb_live_{}", Uuid::new_v4().simple())
    }
}
```

---

## Part C: Rate Limiting

### Strategy: Token Bucket Algorithm

```rust
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct RateLimiter {
    buckets: Arc<Mutex<HashMap<String, TokenBucket>>>,
    default_rate: usize,    // requests per minute
    default_burst: usize,   // burst capacity
}

struct TokenBucket {
    tokens: f64,
    capacity: f64,
    refill_rate: f64,       // tokens per second
    last_update: Instant,
}

impl RateLimiter {
    pub fn new(rate_per_minute: usize, burst: usize) -> Self {
        Self {
            buckets: Arc::new(Mutex::new(HashMap::new())),
            default_rate: rate_per_minute,
            default_burst: burst,
        }
    }

    pub async fn check_rate_limit(&self, key: &str) -> Result<(), RateLimitError> {
        let mut buckets = self.buckets.lock().await;
        
        let bucket = buckets.entry(key.to_string()).or_insert_with(|| {
            TokenBucket {
                tokens: self.default_burst as f64,
                capacity: self.default_burst as f64,
                refill_rate: self.default_rate as f64 / 60.0,
                last_update: Instant::now(),
            }
        });

        bucket.refill();

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            Err(RateLimitError::TooManyRequests)
        }
    }
}
```

### Rate Limit Tiers

| User Role | Requests/Min | Burst | WebSocket Msgs/Min |
|-----------|--------------|-------|-------------------|
| Admin | Unlimited | N/A | Unlimited |
| Trader | 120 | 180 | 300 |
| Viewer | 60 | 90 | 150 |
| Anonymous | 20 | 30 | N/A |

---

## Part D: Role-Based Access Control (RBAC)

### Permission Matrix

| Endpoint | Admin | Trader | Viewer | Anonymous |
|----------|-------|--------|--------|-----------|
| `GET /health` | ✅ | ✅ | ✅ | ✅ |
| `GET /api/signals` | ✅ | ✅ | ✅ | ❌ |
| `GET /api/signals/composite` | ✅ | ✅ | ✅ | ❌ |
| `POST /api/trade` | ✅ | ✅ | ❌ | ❌ |
| `GET /api/risk/stats` | ✅ | ✅ | ✅ | ❌ |
| `POST /api/admin/*` | ✅ | ❌ | ❌ | ❌ |
| `WS /ws` | ✅ | ✅ | ✅ | ❌ |

### Middleware Implementation

```rust
use axum::{
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};

pub async fn auth_middleware<B>(
    State(jwt_handler): State<Arc<JwtHandler>>,
    mut req: Request<B>,
    next: Next<B>,
) -> Result<Response, StatusCode> {
    // Extract token from Authorization header
    let auth_header = req.headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or(StatusCode::UNAUTHORIZED)?;

    // Validate token
    let claims = jwt_handler
        .validate_token(token)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    // Add claims to request extensions
    req.extensions_mut().insert(claims);

    Ok(next.run(req).await)
}

pub fn require_role(required_role: UserRole) -> impl Fn(...) -> ... {
    move |claims: Claims| {
        match (&claims.role, &required_role) {
            (UserRole::Admin, _) => true,  // Admin can do anything
            (role, required) if role == required => true,
            _ => false,
        }
    }
}
```

---

## Part E: Secure WebSocket Connections

### Implementation

```rust
use axum::extract::ws::Message;

async fn secure_websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    // Validate token from query param or header
    let token = headers
        .get("Sec-WebSocket-Protocol")
        .and_then(|h| h.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let claims = state.jwt_handler
        .validate_token(token)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    // Upgrade connection with user context
    ws.on_upgrade(move |socket| {
        handle_authenticated_socket(socket, claims, state)
    })
}
```

---

## Part F: Audit Logging

### Log Security Events

```rust
pub struct AuditLogger {
    db_path: String,
}

#[derive(Debug)]
pub enum AuditEvent {
    Login { username: String, success: bool, ip: String },
    TokenGenerated { user_id: Uuid },
    ApiKeyCreated { key_id: String, user_id: Uuid },
    ApiKeyRevoked { key_id: String },
    RateLimitExceeded { identifier: String },
    UnauthorizedAccess { path: String, ip: String },
}

impl AuditLogger {
    pub async fn log(&self, event: AuditEvent) {
        // Store in audit_log table
        // Format: timestamp, event_type, user_id, details, ip_address
    }
}
```

---

## Implementation Steps

### Step 1: Authentication Core (2 hours)
1. Add dependencies to Cargo.toml
2. Create `rust-backend/src/auth/` module
3. Implement User models
4. Implement JWT handler
5. Create user storage with SQLite
6. Add login endpoint

### Step 2: Middleware & Protection (1 hour)
1. Create auth middleware
2. Create rate limiting middleware
3. Create RBAC middleware
4. Apply to existing endpoints

### Step 3: API Key Management (1 hour)
1. Create API key models
2. Add key generation/validation
3. Add key management endpoints
4. Integrate with auth system

### Step 4: WebSocket Security (1 hour)
1. Add WS authentication
2. Implement token-based upgrade
3. Add per-user message rate limits

### Step 5: Audit Logging (30 min)
1. Create audit log table
2. Implement logger
3. Add logging to critical paths

### Step 6: Testing & Documentation (30 min)
1. Test authentication flow
2. Test rate limiting
3. Test RBAC
4. Document API usage

---

## API Endpoints to Add

### Authentication
- `POST /api/auth/login` - Login with username/password
- `POST /api/auth/refresh` - Refresh JWT token
- `POST /api/auth/logout` - Invalidate token

### User Management (Admin only)
- `POST /api/admin/users` - Create user
- `GET /api/admin/users` - List users
- `DELETE /api/admin/users/:id` - Delete user
- `PUT /api/admin/users/:id/role` - Change role

### API Key Management
- `POST /api/keys` - Generate API key
- `GET /api/keys` - List user's keys
- `DELETE /api/keys/:id` - Revoke key

---

## Environment Variables

```bash
# JWT Configuration
JWT_SECRET=your-super-secret-key-change-in-production
JWT_EXPIRATION_HOURS=24

# Rate Limiting
RATE_LIMIT_PER_MIN=120
RATE_LIMIT_BURST=180

# Database
AUTH_DB_PATH=./betterbot_auth.db
AUDIT_LOG_PATH=./betterbot_audit.db
```

---

## Success Metrics

### Security
- [ ] JWT authentication working
- [ ] All endpoints protected
- [ ] Rate limiting active
- [ ] RBAC enforced
- [ ] WebSockets authenticated
- [ ] Audit logging functional

### Code Quality
- [ ] Clean compilation
- [ ] Unit tests passing
- [ ] Integration tests passing
- [ ] Documentation complete

### Performance
- [ ] Auth overhead <10ms per request
- [ ] Rate limiter <1ms per check
- [ ] No memory leaks
- [ ] Clean shutdown

---

## Testing Strategy

### Unit Tests
```rust
#[test]
fn test_jwt_generation_and_validation() {
    // Test token lifecycle
}

#[test]
fn test_password_hashing() {
    // Test bcrypt
}

#[test]
fn test_rate_limiter() {
    // Test token bucket
}
```

### Integration Tests
```bash
# Login flow
curl -X POST http://localhost:3000/api/auth/login \
  -H "Content-Type: application/json" \
  -d '{"username":"admin","password":"admin123"}'

# Use token
curl http://localhost:3000/api/signals \
  -H "Authorization: Bearer <token>"

# Test rate limit
for i in {1..200}; do
  curl http://localhost:3000/api/signals \
    -H "Authorization: Bearer <token>"
done
```

---

## Security Best Practices

### ✅ Implemented
1. **Bcrypt password hashing** (not plaintext)
2. **JWT with expiration** (24 hours)
3. **Rate limiting** (prevent abuse)
4. **HTTPS recommended** (use reverse proxy)
5. **Audit logging** (track security events)
6. **RBAC** (principle of least privilege)

### ⚠️ Production Checklist
- [ ] Change default admin password
- [ ] Use strong JWT_SECRET (32+ random chars)
- [ ] Enable HTTPS (nginx/Caddy reverse proxy)
- [ ] Set up log rotation
- [ ] Configure firewall rules
- [ ] Regular security audits
- [ ] Dependency vulnerability scanning

---

## Timeline

| Task | Duration | Dependencies |
|------|----------|--------------|
| Auth core | 2h | Phase 6 complete |
| Middleware | 1h | Auth core |
| API keys | 1h | Auth core |
| WS security | 1h | Auth core |
| Audit logging | 30min | All above |
| Testing | 30min | All above |
| **Total** | **6h** | Phase 1-6 complete |

---

## Phase 7 Success Definition

✅ **JWT authentication functional**  
✅ **All endpoints protected**  
✅ **Rate limiting active (120/min traders)**  
✅ **RBAC enforced (Admin/Trader/Viewer)**  
✅ **WebSocket authentication working**  
✅ **Audit logging operational**  
✅ **Default admin user created**  
✅ **API key system working**  
✅ **Clean compilation & tests passing**  
✅ **Documentation comprehensive**  

---

## Why Security Matters

### Without Phase 7
- ❌ Anyone can access all endpoints
- ❌ No usage limits → DDoS vulnerable
- ❌ No audit trail → can't track attacks
- ❌ No access control → data exposure risk
- **NOT PRODUCTION READY**

### With Phase 7
- ✅ Only authenticated users access data
- ✅ Rate limits prevent abuse
- ✅ Audit logs track all activity
- ✅ RBAC prevents unauthorized actions
- **PRODUCTION READY** 🚀

---

*"Security is not a product, but a process."*  
— Bruce Schneier

**Let's make BetterBot production-secure. 🔐**
