//! Authentication Middleware
//! Mission: Protect API endpoints with JWT validation

use crate::auth::{jwt::JwtHandler, models::Claims};
use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;

/// Auth middleware that validates JWT tokens
pub async fn auth_middleware(
    State(jwt_handler): State<Arc<JwtHandler>>,
    mut req: Request,
    next: Next,
) -> Result<Response, AuthError> {
    // First, check for token in query parameters (for WebSockets)
    // Example: /ws?token=...
    let token_from_query = if let Some(query) = req.uri().query() {
        query
            .split('&')
            .find(|pair| pair.starts_with("token="))
            .and_then(|pair| pair.split('=').nth(1))
            .map(|t| t.to_string())
    } else {
        None
    };

    // Second, check for Authorization header (Bearer ...)
    let token_from_header = req
        .headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|t| t.to_string());

    // Use whichever token was found
    let token = token_from_query
        .or(token_from_header)
        .ok_or(AuthError::MissingToken)?;

    // Validate token and extract claims
    let claims = jwt_handler
        .validate_token(&token)
        .map_err(|_| AuthError::InvalidToken)?;

    // Add claims to request extensions so handlers can access them
    req.extensions_mut().insert(claims);

    // Continue to next handler
    Ok(next.run(req).await)
}

/// Optional auth middleware - allows requests without token but adds claims if present
pub async fn optional_auth_middleware(
    State(jwt_handler): State<Arc<JwtHandler>>,
    mut req: Request,
    next: Next,
) -> Response {
    // Try to extract token but don't fail if missing
    if let Some(auth_header) = req.headers().get("Authorization") {
        if let Ok(auth_str) = auth_header.to_str() {
            if let Some(token) = auth_str.strip_prefix("Bearer ") {
                // Try to validate token
                if let Ok(claims) = jwt_handler.validate_token(token) {
                    req.extensions_mut().insert(claims);
                }
            }
        }
    }

    next.run(req).await
}

/// Extract claims from request (use after auth middleware)
pub fn extract_claims(req: &Request) -> Option<&Claims> {
    req.extensions().get::<Claims>()
}

/// Auth error types
#[derive(Debug)]
pub enum AuthError {
    MissingToken,
    InvalidFormat,
    InvalidToken,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            AuthError::MissingToken => (StatusCode::UNAUTHORIZED, "Missing authorization token"),
            AuthError::InvalidFormat => (
                StatusCode::UNAUTHORIZED,
                "Invalid authorization format. Use: Bearer {token}",
            ),
            AuthError::InvalidToken => (StatusCode::UNAUTHORIZED, "Invalid or expired token"),
        };

        (status, message).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{models::User, models::UserRole};
    use axum::{body::Body, http::Request as HttpRequest};
    use uuid::Uuid;

    fn create_test_user() -> User {
        User {
            id: Uuid::new_v4(),
            username: "testuser".to_string(),
            password_hash: "hash".to_string(),
            role: UserRole::Trader,
            api_key: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    #[test]
    fn test_auth_error_responses() {
        let missing = AuthError::MissingToken.into_response();
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

        let invalid_format = AuthError::InvalidFormat.into_response();
        assert_eq!(invalid_format.status(), StatusCode::UNAUTHORIZED);

        let invalid_token = AuthError::InvalidToken.into_response();
        assert_eq!(invalid_token.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_extract_claims_from_request() {
        let mut req = HttpRequest::new(Body::empty());

        // No claims initially
        assert!(extract_claims(&req).is_none());

        // Add claims
        let claims = Claims {
            sub: Uuid::new_v4().to_string(),
            username: "test".to_string(),
            role: UserRole::Trader,
            exp: 1234567890,
        };
        req.extensions_mut().insert(claims.clone());

        // Should be able to extract
        let extracted = extract_claims(&req);
        assert!(extracted.is_some());
        assert_eq!(extracted.unwrap().username, "test");
    }
}
