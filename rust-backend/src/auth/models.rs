//! Authentication Models
//! Mission: Define secure user and authentication data structures

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// User account
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    #[serde(skip_serializing)]
    pub password_hash: String, // bcrypt hash - never serialize
    pub role: UserRole,
    pub api_key: Option<String>,
    pub created_at: String,
}

/// User roles for RBAC
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum UserRole {
    #[serde(rename = "admin")]
    Admin, // Full access to all endpoints
    #[serde(rename = "trader")]
    Trader, // Signal access + trading operations
    #[serde(rename = "viewer")]
    Viewer, // Read-only access
}

impl UserRole {
    pub fn as_str(&self) -> &str {
        match self {
            UserRole::Admin => "admin",
            UserRole::Trader => "trader",
            UserRole::Viewer => "viewer",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "admin" => Some(UserRole::Admin),
            "trader" => Some(UserRole::Trader),
            "viewer" => Some(UserRole::Viewer),
            _ => None,
        }
    }
}

/// JWT Claims payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String, // subject (user_id)
    pub username: String,
    pub role: UserRole,
    pub exp: usize, // expiration timestamp
}

/// Login request body
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

/// Login response
#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub expires_in: usize, // seconds until expiration
    pub role: UserRole,
    pub user: UserResponse,
}

/// User response (sanitized)
#[derive(Debug, Serialize)]
pub struct UserResponse {
    pub id: String,
    pub username: String,
    pub role: UserRole,
    pub created_at: String,
}

impl UserResponse {
    pub fn from_user(user: &User) -> Self {
        Self {
            id: user.id.to_string(),
            username: user.username.clone(),
            role: user.role.clone(),
            created_at: user.created_at.clone(),
        }
    }
}

/// API Key for programmatic access
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    pub id: Uuid,
    pub key: String, // "btb_live_xxxxxxxxxxxx"
    pub user_id: Uuid,
    pub name: String,      // Descriptive name
    pub rate_limit: usize, // requests per minute
    pub created_at: String,
    pub last_used: Option<String>,
    pub revoked: bool,
}

impl ApiKey {
    /// Generate a new API key string
    pub fn generate_key() -> String {
        format!("btb_live_{}", Uuid::new_v4().simple())
    }
}

/// API Key creation request
#[derive(Debug, Deserialize)]
pub struct CreateApiKeyRequest {
    pub name: String,
    pub rate_limit: Option<usize>, // Optional custom rate limit
}

/// API Key response (sanitized)
#[derive(Debug, Serialize)]
pub struct ApiKeyResponse {
    pub id: Uuid,
    pub key: String, // Only shown once during creation
    pub name: String,
    pub rate_limit: usize,
    pub created_at: String,
}

/// Rate limit error
#[derive(Debug)]
pub enum RateLimitError {
    TooManyRequests,
    InvalidKey,
}

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RateLimitError::TooManyRequests => write!(f, "Rate limit exceeded"),
            RateLimitError::InvalidKey => write!(f, "Invalid API key"),
        }
    }
}

impl std::error::Error for RateLimitError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_role_serialization() {
        let admin = UserRole::Admin;
        let json = serde_json::to_string(&admin).unwrap();
        assert_eq!(json, r#""admin""#);

        let trader: UserRole = serde_json::from_str(r#""trader""#).unwrap();
        assert_eq!(trader, UserRole::Trader);
    }

    #[test]
    fn test_api_key_generation() {
        let key1 = ApiKey::generate_key();
        let key2 = ApiKey::generate_key();

        assert!(key1.starts_with("btb_live_"));
        assert!(key2.starts_with("btb_live_"));
        assert_ne!(key1, key2); // Keys should be unique
    }

    #[test]
    fn test_user_role_string_conversion() {
        assert_eq!(UserRole::Admin.as_str(), "admin");
        assert_eq!(UserRole::Trader.as_str(), "trader");
        assert_eq!(UserRole::Viewer.as_str(), "viewer");

        assert_eq!(UserRole::from_str("admin"), Some(UserRole::Admin));
        assert_eq!(UserRole::from_str("TRADER"), Some(UserRole::Trader));
        assert_eq!(UserRole::from_str("invalid"), None);
    }
}
