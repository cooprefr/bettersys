//! Authentication Module
//! Mission: Secure API access with JWT tokens, RBAC, and rate limiting

pub mod api;
pub mod jwt;
pub mod middleware;
pub mod models;
pub mod user_store;

pub use api::AuthState;
pub use jwt::JwtHandler;
pub use middleware::auth_middleware;
pub use user_store::UserStore;
