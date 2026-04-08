use std::fmt;

use axum_login::{AuthUser, AuthnBackend, UserId};
use bcrypt::{hash, verify, DEFAULT_COST};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::db::models::User;

#[derive(Debug)]
pub enum AuthError {
    Database(sqlx::Error),
    Bcrypt(bcrypt::BcryptError),
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthError::Database(e) => write!(f, "Database error: {}", e),
            AuthError::Bcrypt(e) => write!(f, "Bcrypt error: {}", e),
        }
    }
}

impl std::error::Error for AuthError {}

impl From<sqlx::Error> for AuthError {
    fn from(e: sqlx::Error) -> Self {
        AuthError::Database(e)
    }
}

impl From<bcrypt::BcryptError> for AuthError {
    fn from(e: bcrypt::BcryptError) -> Self {
        AuthError::Bcrypt(e)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone)]
pub struct PasswordBackend {
    db: SqlitePool,
}

impl PasswordBackend {
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }

    pub async fn create_user(
        &self,
        username: &str,
        password: &str,
        email: Option<&str>,
        is_admin: bool,
    ) -> Result<User, AuthError> {
        let id = generate_id();
        let now = chrono::Utc::now().timestamp();
        let password_hash = hash(password.as_bytes(), DEFAULT_COST)?;

        let user = sqlx::query_as::<_, User>(
            r#"
            INSERT INTO users (id, username, password_hash, email, is_admin, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            RETURNING *
            "#,
        )
        .bind(&id)
        .bind(username)
        .bind(&password_hash)
        .bind(email)
        .bind(is_admin)
        .bind(now)
        .bind(now)
        .fetch_one(&self.db)
        .await?;

        Ok(user)
    }

    pub async fn get_user_by_id(&self, id: &str) -> Result<Option<User>, AuthError> {
        let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.db)
            .await?;

        Ok(user)
    }
}

impl AuthnBackend for PasswordBackend {
    type User = User;
    type Credentials = Credentials;
    type Error = AuthError;

    async fn authenticate(
        &self,
        creds: Self::Credentials,
    ) -> Result<Option<Self::User>, Self::Error> {
        let user: Option<User> =
            sqlx::query_as("SELECT * FROM users WHERE username = ?")
                .bind(&creds.username)
                .fetch_optional(&self.db)
                .await?;

        if let Some(user) = user {
            if verify(creds.password.as_bytes(), &user.password_hash)? {
                return Ok(Some(user));
            }
        }

        Ok(None)
    }

    async fn get_user(&self, user_id: &UserId<Self>) -> Result<Option<Self::User>, Self::Error> {
        self.get_user_by_id(user_id).await
    }
}

impl AuthUser for User {
    type Id = String;

    fn id(&self) -> Self::Id {
        self.id.clone()
    }

    fn session_auth_hash(&self) -> &[u8] {
        self.password_hash.as_bytes()
    }
}

fn generate_id() -> String {
    let mut rng = rand::rng();
    let bytes: [u8; 16] = rng.random();
    hex::encode(bytes)
}
