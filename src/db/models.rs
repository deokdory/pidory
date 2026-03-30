use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Project {
    pub channel_id: String,
    pub path: String,
    pub name: Option<String>,
    pub disallowed_tools: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Session {
    pub thread_id: String,
    pub channel_id: String,
    pub session_id: Option<String>,
    pub status: String,
    pub created_at: String,
    pub last_active_at: Option<String>,
}
