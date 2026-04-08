use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use axum_login::{AuthSession, AuthnBackend};
use serde::{Deserialize, Serialize};

use crate::auth::{backend::Credentials, backend::PasswordBackend};
use crate::db::models::User;
use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub user: UserResponse,
}

#[derive(Debug, Serialize)]
pub struct UserResponse {
    pub id: String,
    pub username: String,
    pub email: Option<String>,
    pub is_admin: bool,
}

impl From<User> for UserResponse {
    fn from(user: User) -> Self {
        Self {
            id: user.id,
            username: user.username,
            email: user.email,
            is_admin: user.is_admin,
        }
    }
}

pub async fn login(
    mut auth_session: AuthSession<PasswordBackend>,
    Json(creds): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, StatusCode> {
    tracing::info!("Login attempt for user: {}", creds.username);
    
    let credentials = Credentials {
        username: creds.username,
        password: creds.password,
    };

    let user = match auth_session.authenticate(credentials).await {
        Ok(Some(user)) => {
            tracing::info!("User authenticated successfully");
            user
        }
        Ok(None) => {
            tracing::info!("Authentication failed: invalid credentials");
            return Err(StatusCode::UNAUTHORIZED);
        }
        Err(e) => {
            tracing::error!("Login authentication error: {:?}", e);
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    if auth_session.login(&user).await.is_err() {
        tracing::error!("Failed to create session");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    tracing::info!("Login successful for user: {}", user.username);
    Ok(Json(LoginResponse {
        user: user.into(),
    }))
}

pub async fn logout(mut auth_session: AuthSession<PasswordBackend>) -> impl IntoResponse {
    match auth_session.logout().await {
        Ok(_) => StatusCode::OK,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
    pub email: Option<String>,
}

pub async fn register(
    mut auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<LoginResponse>, StatusCode> {
    // Only allow registration when no users exist (initial admin setup)
    let user_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if user_count > 0 {
        return Err(StatusCode::FORBIDDEN);
    }

    let backend = PasswordBackend::new(state.db.clone());

    let user = match backend
        .create_user(&req.username, &req.password, req.email.as_deref(), true)
        .await
    {
        Ok(u) => u,
        Err(e) => {
            tracing::error!("Register create user error: {:?}", e);
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };

    // Auto-login after registration
    if auth_session.login(&user).await.is_err() {
        tracing::error!("Failed to create session after registration");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    Ok(Json(LoginResponse {
        user: user.into(),
    }))
}

#[derive(Debug, Serialize)]
pub struct SetupResponse {
    pub needs_setup: bool,
}

pub async fn setup_check(
    State(state): State<AppState>,
) -> Result<Json<SetupResponse>, StatusCode> {
    let user_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&state.db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(SetupResponse {
        needs_setup: user_count == 0,
    }))
}

pub async fn me(auth_session: AuthSession<PasswordBackend>) -> Result<Json<UserResponse>, StatusCode> {
    match auth_session.user {
        Some(user) => Ok(Json(user.into())),
        None => Err(StatusCode::UNAUTHORIZED),
    }
}

#[derive(Debug, Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Debug, Serialize)]
pub struct ChangePasswordResponse {
    pub success: bool,
}

pub async fn change_password(
    auth_session: AuthSession<PasswordBackend>,
    State(state): State<AppState>,
    Json(req): Json<ChangePasswordRequest>,
) -> Result<Json<ChangePasswordResponse>, StatusCode> {
    let user = match &auth_session.user {
        Some(user) => user,
        None => return Err(StatusCode::UNAUTHORIZED),
    };

    // Verify current password
    let credentials = Credentials {
        username: user.username.clone(),
        password: req.current_password,
    };

    let backend = PasswordBackend::new(state.db.clone());
    match backend.authenticate(credentials).await {
        Ok(Some(_)) => {
            // Current password is correct, update to new password
            let new_hash = bcrypt::hash(req.new_password.as_bytes(), bcrypt::DEFAULT_COST)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            
            let now = chrono::Utc::now().timestamp();
            sqlx::query(
                "UPDATE users SET password_hash = ?, updated_at = ? WHERE id = ?"
            )
            .bind(&new_hash)
            .bind(now)
            .bind(&user.id)
            .execute(&state.db)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

            Ok(Json(ChangePasswordResponse { success: true }))
        }
        Ok(None) => Err(StatusCode::UNAUTHORIZED), // Wrong current password
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}
