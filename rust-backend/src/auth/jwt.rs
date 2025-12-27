//! JWT Token Handler
//! Mission: Generate and validate JWT tokens securely

use crate::auth::models::{Claims, User};
use anyhow::{Context, Result};
use chrono::Utc;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use tracing::debug;

/// JWT Handler for token operations
pub struct JwtHandler {
    secret: String,
    expiration_hours: i64,
}

impl JwtHandler {
    /// Create a new JWT handler with secret key
    pub fn new(secret: String) -> Self {
        Self {
            secret,
            expiration_hours: 24, // 24-hour tokens by default
        }
    }

    /// Generate a JWT token for a user
    pub fn generate_token(&self, user: &User) -> Result<(String, usize)> {
        let now = Utc::now();
        let expiration = now
            .checked_add_signed(chrono::Duration::hours(self.expiration_hours))
            .context("Invalid timestamp")?
            .timestamp() as usize;

        let expires_in = (self.expiration_hours * 3600) as usize;

        let claims = Claims {
            sub: user.id.to_string(),
            username: user.username.clone(),
            role: user.role.clone(),
            exp: expiration,
        };

        debug!(
            "Generating JWT for user {} ({}), expires in {}h",
            user.username, user.id, self.expiration_hours
        );

        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(self.secret.as_bytes()),
        )
        .context("Failed to generate JWT")?;

        Ok((token, expires_in))
    }

    /// Validate a JWT token and extract claims
    pub fn validate_token(&self, token: &str) -> Result<Claims> {
        let decoded = decode::<Claims>(
            token,
            &DecodingKey::from_secret(self.secret.as_bytes()),
            &Validation::default(),
        )
        .context("Invalid or expired token")?;

        debug!("Validated JWT for user {}", decoded.claims.username);

        Ok(decoded.claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::models::UserRole;
    use uuid::Uuid;

    fn create_test_user() -> User {
        User {
            id: Uuid::new_v4(),
            username: "testuser".to_string(),
            password_hash: "hash".to_string(),
            role: UserRole::Trader,
            api_key: None,
            created_at: Utc::now().to_rfc3339(),
        }
    }

    #[test]
    fn test_jwt_generation_and_validation() {
        let handler = JwtHandler::new("test-secret-key-12345".to_string());
        let user = create_test_user();

        // Generate token
        let (token, expires_in) = handler.generate_token(&user).unwrap();
        assert!(!token.is_empty());
        assert_eq!(expires_in, 24 * 3600); // 24 hours in seconds

        // Validate token
        let claims = handler.validate_token(&token).unwrap();
        assert_eq!(claims.username, user.username);
        assert_eq!(claims.sub, user.id.to_string());
        assert_eq!(claims.role, user.role);
    }

    #[test]
    fn test_invalid_token_rejected() {
        let handler = JwtHandler::new("test-secret-key-12345".to_string());

        // Try to validate invalid token
        let result = handler.validate_token("invalid.token.here");
        assert!(result.is_err());
    }

    #[test]
    fn test_different_secrets_reject() {
        let handler1 = JwtHandler::new("secret1".to_string());
        let handler2 = JwtHandler::new("secret2".to_string());
        let user = create_test_user();

        // Generate with handler1
        let (token, _) = handler1.generate_token(&user).unwrap();

        // Try to validate with handler2 (different secret)
        let result = handler2.validate_token(&token);
        assert!(result.is_err());
    }

    #[test]
    fn test_token_contains_all_claims() {
        let handler = JwtHandler::new("test-secret-key-12345".to_string());
        let user = User {
            id: Uuid::new_v4(),
            username: "admin".to_string(),
            password_hash: "hash".to_string(),
            role: UserRole::Admin,
            api_key: None,
            created_at: Utc::now().to_rfc3339(),
        };

        let (token, _) = handler.generate_token(&user).unwrap();
        let claims = handler.validate_token(&token).unwrap();

        assert_eq!(claims.username, "admin");
        assert_eq!(claims.role, UserRole::Admin);
        assert!(claims.exp > Utc::now().timestamp() as usize);
    }
}
