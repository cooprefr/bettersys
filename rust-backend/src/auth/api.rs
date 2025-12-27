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
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, warn};
use uuid::Uuid;

/// Shared auth state
#[derive(Clone)]
pub struct AuthState {
    pub user_store: Arc<UserStore>,
    pub jwt_handler: Arc<JwtHandler>,
}

impl AuthState {
    pub fn new(user_store: Arc<UserStore>, jwt_handler: Arc<JwtHandler>) -> Self {
        Self {
            user_store,
            jwt_handler,
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
