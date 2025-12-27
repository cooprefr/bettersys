//! Authentication API Endpoints
//! Mission: Provide login and user management endpoints

use crate::auth::{
    jwt::JwtHandler,
    middleware::extract_claims,
    models::{LoginRequest, LoginResponse, User, UserResponse, UserRole},
    user_store::UserStore,
};
use axum::{
    extract::{Path, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use jsonwebtoken::{decode, decode_header, jwk::JwkSet, Algorithm, DecodingKey, Validation};
use num_bigint::BigUint;
use serde::Deserialize;
use serde_json::json;
use std::env;
use std::sync::Arc;
use tracing::{info, warn};
use uuid::Uuid;

/// Shared auth state
#[derive(Clone)]
pub struct AuthState {
    pub user_store: Arc<UserStore>,
    pub jwt_handler: Arc<JwtHandler>,

    // Shared HTTP client for outbound calls (Privy + RPC)
    pub http_client: reqwest::Client,

    // Privy config
    pub privy_app_id: String,
    pub privy_jwks_url: String,
    pub privy_issuer: String,

    // Token gate config
    pub token_gate_enabled: bool,
    pub base_rpc_url: Option<String>,
    pub better_token_address: Option<String>,
    pub better_token_decimals: u32,
    pub better_min_balance: u64,
}

impl AuthState {
    pub fn new(
        user_store: Arc<UserStore>,
        jwt_handler: Arc<JwtHandler>,
        http_client: reqwest::Client,
    ) -> Self {
        let privy_app_id = env::var("PRIVY_APP_ID").unwrap_or_default();
        let privy_jwks_url = env::var("PRIVY_JWKS_URL")
            .unwrap_or_else(|_| "https://auth.privy.io/jwks.json".to_string());
        let privy_issuer = env::var("PRIVY_ISSUER").unwrap_or_else(|_| "privy.io".to_string());

        let token_gate_enabled = env::var("BETTER_TOKEN_GATE_ENABLED")
            .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "on" | "ON"))
            .unwrap_or(true);

        let base_rpc_url = env::var("BASE_RPC_URL")
            .or_else(|_| env::var("BASE_RPC_HTTP_URL"))
            .ok();

        let better_token_address = env::var("BETTER_TOKEN_ADDRESS").ok();

        let better_token_decimals = env::var("BETTER_TOKEN_DECIMALS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(18);

        let better_min_balance = env::var("BETTER_TOKEN_MIN_BALANCE")
            .or_else(|_| env::var("BETTER_MIN_BALANCE"))
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(100_000);

        Self {
            user_store,
            jwt_handler,
            http_client,
            privy_app_id,
            privy_jwks_url,
            privy_issuer,
            token_gate_enabled,
            base_rpc_url,
            better_token_address,
            better_token_decimals,
            better_min_balance,
        }
    }
}

/// Login endpoint - POST /api/auth/login
pub async fn login(
    State(state): State<AuthState>,
    Json(payload): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, AuthApiError> {
    info!("🔐 Login attempt: {}", payload.username);

    // Verify credentials
    let valid = state
        .user_store
        .verify_password(&payload.username, &payload.password)
        .map_err(|_| AuthApiError::InternalError)?;

    if !valid {
        warn!("❌ Failed login attempt: {}", payload.username);
        return Err(AuthApiError::InvalidCredentials);
    }

    // Get user details
    let user = state
        .user_store
        .get_user_by_username(&payload.username)
        .map_err(|_| AuthApiError::InternalError)?
        .ok_or(AuthApiError::InvalidCredentials)?;

    // Generate JWT token
    let (token, expires_in) = state
        .jwt_handler
        .generate_token(&user)
        .map_err(|_| AuthApiError::InternalError)?;

    info!(
        "✅ Login successful: {} ({})",
        user.username,
        user.role.as_str()
    );

    Ok(Json(LoginResponse {
        token,
        expires_in,
        role: user.role.clone(),
        user: UserResponse::from_user(&user),
    }))
}

/// Privy login endpoint - POST /api/auth/privy
/// Expects a Privy identity token (ES256 JWT) and enforces Base ERC-20 token gating.
pub async fn privy_login(
    State(state): State<AuthState>,
    Json(payload): Json<PrivyLoginRequest>,
) -> Result<Json<LoginResponse>, AuthApiError> {
    if state.privy_app_id.trim().is_empty() {
        return Err(AuthApiError::PrivyNotConfigured);
    }

    let claims = verify_privy_identity_token(&state, &payload.identity_token).await?;
    let wallets = extract_evm_wallets(&claims);
    if wallets.is_empty() {
        return Err(AuthApiError::PrivyNoWallet);
    }

    let selected_wallet = enforce_better_token_gate(&state, &wallets).await?;

    let user = User {
        id: Uuid::new_v5(&Uuid::NAMESPACE_URL, claims.sub.as_bytes()),
        username: selected_wallet,
        password_hash: String::new(),
        role: UserRole::Trader,
        api_key: None,
        created_at: Utc::now().to_rfc3339(),
    };

    let (token, expires_in) = state
        .jwt_handler
        .generate_token(&user)
        .map_err(|_| AuthApiError::InternalError)?;

    Ok(Json(LoginResponse {
        token,
        expires_in,
        role: user.role.clone(),
        user: UserResponse::from_user(&user),
    }))
}

#[derive(Debug, Deserialize)]
pub struct PrivyLoginRequest {
    pub identity_token: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrivyIdentityClaims {
    pub sub: String,
    #[serde(rename = "linkedAccounts")]
    pub linked_accounts: Option<Vec<PrivyLinkedAccount>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrivyLinkedAccount {
    #[serde(rename = "type")]
    pub kind: String,
    pub address: Option<String>,
    pub chain_type: Option<String>,
}

async fn verify_privy_identity_token(
    state: &AuthState,
    identity_token: &str,
) -> Result<PrivyIdentityClaims, AuthApiError> {
    let header = decode_header(identity_token).map_err(|_| AuthApiError::PrivyInvalidToken)?;
    let kid = header.kid.ok_or(AuthApiError::PrivyInvalidToken)?;

    let jwks = fetch_privy_jwks(state).await?;
    let jwk = jwks
        .keys
        .iter()
        .find(|k| k.common.key_id.as_deref() == Some(kid.as_str()))
        .ok_or(AuthApiError::PrivyInvalidToken)?;

    let decoding_key = DecodingKey::from_jwk(jwk).map_err(|_| AuthApiError::PrivyInvalidToken)?;
    let mut validation = Validation::new(Algorithm::ES256);
    validation.set_audience(std::slice::from_ref(&state.privy_app_id));
    validation.set_issuer(std::slice::from_ref(&state.privy_issuer));

    let token_data = decode::<PrivyIdentityClaims>(identity_token, &decoding_key, &validation)
        .map_err(|_| AuthApiError::PrivyInvalidToken)?;
    Ok(token_data.claims)
}

async fn fetch_privy_jwks(state: &AuthState) -> Result<JwkSet, AuthApiError> {
    let resp = state
        .http_client
        .get(&state.privy_jwks_url)
        .send()
        .await
        .map_err(|_| AuthApiError::PrivyJwksFetchFailed)?;

    if !resp.status().is_success() {
        return Err(AuthApiError::PrivyJwksFetchFailed);
    }

    resp.json::<JwkSet>()
        .await
        .map_err(|_| AuthApiError::PrivyJwksFetchFailed)
}

fn extract_evm_wallets(claims: &PrivyIdentityClaims) -> Vec<String> {
    let mut out = Vec::new();
    let Some(accounts) = &claims.linked_accounts else {
        return out;
    };

    for acc in accounts {
        let Some(addr) = acc.address.as_deref() else {
            continue;
        };
        let addr = addr.trim();
        if !addr.starts_with("0x") || addr.len() != 42 {
            continue;
        }

        // Prefer EVM wallets; if chain_type is present, filter down.
        if let Some(chain_type) = acc.chain_type.as_deref() {
            let ct = chain_type.to_ascii_lowercase();
            if ct != "ethereum" && ct != "eip155" {
                continue;
            }
        }

        if !acc.kind.eq_ignore_ascii_case("wallet") {
            continue;
        }

        out.push(addr.to_string());
    }

    out
}

async fn enforce_better_token_gate(
    state: &AuthState,
    wallets: &[String],
) -> Result<String, AuthApiError> {
    if !state.token_gate_enabled {
        return wallets.first().cloned().ok_or(AuthApiError::PrivyNoWallet);
    }

    let base_rpc_url = state
        .base_rpc_url
        .as_deref()
        .ok_or(AuthApiError::TokenGateNotConfigured)?;
    let token_address = state
        .better_token_address
        .as_deref()
        .ok_or(AuthApiError::TokenGateNotConfigured)?;

    let min_required = min_balance_wei(state.better_min_balance, state.better_token_decimals);

    for w in wallets {
        let bal = erc20_balance_of(&state.http_client, base_rpc_url, token_address, w).await?;
        if bal >= min_required {
            return Ok(w.clone());
        }
    }

    Err(AuthApiError::TokenGateFailed)
}

fn min_balance_wei(min_balance: u64, decimals: u32) -> BigUint {
    let base = BigUint::from(10u32);
    BigUint::from(min_balance) * base.pow(decimals)
}

#[derive(Debug, Deserialize)]
struct RpcErrorObject {
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RpcResponse {
    pub result: Option<String>,
    pub error: Option<RpcErrorObject>,
}

async fn erc20_balance_of(
    http: &reqwest::Client,
    rpc_url: &str,
    token_address: &str,
    wallet_address: &str,
) -> Result<BigUint, AuthApiError> {
    let wallet = wallet_address
        .trim()
        .trim_start_matches("0x")
        .to_ascii_lowercase();
    if wallet.len() != 40 {
        return Err(AuthApiError::TokenGateRpcFailed);
    }
    let token = token_address.trim();
    if !token.starts_with("0x") || token.len() != 42 {
        return Err(AuthApiError::TokenGateNotConfigured);
    }

    // balanceOf(address) -> 0x70a08231
    let data = format!("0x70a08231{:0>64}", wallet);
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_call",
        "params": [
            { "to": token, "data": data },
            "latest"
        ]
    });

    let resp = http
        .post(rpc_url)
        .json(&body)
        .send()
        .await
        .map_err(|_| AuthApiError::TokenGateRpcFailed)?;

    if !resp.status().is_success() {
        return Err(AuthApiError::TokenGateRpcFailed);
    }

    let rpc = resp
        .json::<RpcResponse>()
        .await
        .map_err(|_| AuthApiError::TokenGateRpcFailed)?;

    if rpc.error.is_some() {
        return Err(AuthApiError::TokenGateRpcFailed);
    }

    let Some(result) = rpc.result else {
        return Err(AuthApiError::TokenGateRpcFailed);
    };
    let hex = result.trim().trim_start_matches("0x");
    let bal = BigUint::parse_bytes(hex.as_bytes(), 16).unwrap_or_else(|| BigUint::from(0u32));
    Ok(bal)
}

/// Get current user info - GET /api/auth/me
/// This endpoint extracts user info from the JWT token (no database lookup needed)
pub async fn get_current_user(req: Request) -> Result<Json<LoginResponse>, AuthApiError> {
    let claims = extract_claims(&req).ok_or(AuthApiError::Unauthorized)?;

    // Build response from JWT claims (no database lookup needed)
    Ok(Json(LoginResponse {
        token: String::new(), // Not included in /me response
        expires_in: 0,
        role: claims.role.clone(),
        user: UserResponse {
            id: claims.sub.clone(),
            username: claims.username.clone(),
            role: claims.role.clone(),
            created_at: String::new(),
        },
    }))
}

/// List all users - GET /api/admin/users (Admin only)
pub async fn list_users(
    State(state): State<AuthState>,
    req: Request,
) -> Result<Json<Vec<UserResponse>>, AuthApiError> {
    let claims = extract_claims(&req).ok_or(AuthApiError::Unauthorized)?;

    // Check admin role
    if claims.role != UserRole::Admin {
        return Err(AuthApiError::Forbidden);
    }

    let users = state
        .user_store
        .list_users()
        .map_err(|_| AuthApiError::InternalError)?;

    let response: Vec<UserResponse> = users.iter().map(UserResponse::from_user).collect();

    Ok(Json(response))
}

/// Create user - POST /api/admin/users (Admin only)
pub async fn create_user(
    State(state): State<AuthState>,
    req: Request,
    Json(payload): Json<CreateUserRequest>,
) -> Result<Json<UserResponse>, AuthApiError> {
    let claims = extract_claims(&req).ok_or(AuthApiError::Unauthorized)?;

    // Check admin role
    if claims.role != UserRole::Admin {
        return Err(AuthApiError::Forbidden);
    }

    // Validate password length
    if payload.password.len() < 8 {
        return Err(AuthApiError::WeakPassword);
    }

    // Create user
    let user = state
        .user_store
        .create_user(&payload.username, &payload.password, payload.role)
        .map_err(|e| {
            warn!("Failed to create user: {}", e);
            AuthApiError::UserAlreadyExists
        })?;

    info!(
        "✅ User created: {} ({})",
        user.username,
        user.role.as_str()
    );

    Ok(Json(UserResponse::from_user(&user)))
}

/// Delete user - DELETE /api/admin/users/:id (Admin only)
pub async fn delete_user(
    State(state): State<AuthState>,
    req: Request,
    Path(user_id): Path<String>,
) -> Result<StatusCode, AuthApiError> {
    let claims = extract_claims(&req).ok_or(AuthApiError::Unauthorized)?;

    // Check admin role
    if claims.role != UserRole::Admin {
        return Err(AuthApiError::Forbidden);
    }

    // Parse UUID
    let uuid = Uuid::parse_str(&user_id).map_err(|_| AuthApiError::InvalidUserId)?;

    // Don't allow deleting yourself
    if uuid.to_string() == claims.sub {
        return Err(AuthApiError::CannotDeleteSelf);
    }

    // Delete user
    state
        .user_store
        .delete_user(&uuid)
        .map_err(|_| AuthApiError::UserNotFound)?;

    info!("🗑️  User deleted: {}", user_id);

    Ok(StatusCode::NO_CONTENT)
}

/// Create user request
#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    pub password: String,
    pub role: UserRole,
}

/// Auth API errors
#[derive(Debug)]
pub enum AuthApiError {
    InvalidCredentials,
    Unauthorized,
    Forbidden,
    PrivyNotConfigured,
    PrivyInvalidToken,
    PrivyJwksFetchFailed,
    PrivyNoWallet,
    TokenGateNotConfigured,
    TokenGateFailed,
    TokenGateRpcFailed,
    UserNotFound,
    UserAlreadyExists,
    WeakPassword,
    InvalidUserId,
    CannotDeleteSelf,
    InternalError,
}

impl IntoResponse for AuthApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            AuthApiError::InvalidCredentials => {
                (StatusCode::UNAUTHORIZED, "Invalid username or password")
            }
            AuthApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "Authentication required"),
            AuthApiError::Forbidden => (StatusCode::FORBIDDEN, "Insufficient permissions"),
            AuthApiError::PrivyNotConfigured => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Privy login not configured",
            ),
            AuthApiError::PrivyInvalidToken => (StatusCode::UNAUTHORIZED, "Invalid Privy token"),
            AuthApiError::PrivyJwksFetchFailed => (
                StatusCode::BAD_GATEWAY,
                "Failed to fetch Privy verification keys",
            ),
            AuthApiError::PrivyNoWallet => (
                StatusCode::BAD_REQUEST,
                "Privy user has no linked EVM wallet",
            ),
            AuthApiError::TokenGateNotConfigured => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Token gate not configured",
            ),
            AuthApiError::TokenGateFailed => (
                StatusCode::FORBIDDEN,
                "Insufficient $BETTER balance for access",
            ),
            AuthApiError::TokenGateRpcFailed => {
                (StatusCode::BAD_GATEWAY, "Failed to check token balance")
            }
            AuthApiError::UserNotFound => (StatusCode::NOT_FOUND, "User not found"),
            AuthApiError::UserAlreadyExists => (StatusCode::CONFLICT, "Username already exists"),
            AuthApiError::WeakPassword => (
                StatusCode::BAD_REQUEST,
                "Password must be at least 8 characters",
            ),
            AuthApiError::InvalidUserId => (StatusCode::BAD_REQUEST, "Invalid user ID format"),
            AuthApiError::CannotDeleteSelf => {
                (StatusCode::BAD_REQUEST, "Cannot delete your own account")
            }
            AuthApiError::InternalError => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error")
            }
        };

        (status, message).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_response_from_user() {
        let user = User {
            id: Uuid::new_v4(),
            username: "testuser".to_string(),
            password_hash: "hash123".to_string(),
            role: UserRole::Trader,
            api_key: None,
            created_at: "2025-01-01T00:00:00Z".to_string(),
        };

        let response = UserResponse::from_user(&user);
        assert_eq!(response.username, "testuser");
        assert_eq!(response.role, UserRole::Trader);
        // Password hash should not be in response
    }

    #[test]
    fn test_auth_api_error_responses() {
        let invalid_creds = AuthApiError::InvalidCredentials.into_response();
        assert_eq!(invalid_creds.status(), StatusCode::UNAUTHORIZED);

        let forbidden = AuthApiError::Forbidden.into_response();
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);

        let not_found = AuthApiError::UserNotFound.into_response();
        assert_eq!(not_found.status(), StatusCode::NOT_FOUND);

        let conflict = AuthApiError::UserAlreadyExists.into_response();
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
    }
}
