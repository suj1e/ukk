//! ukk SQLite repository API. Runtime processes remain owned by herdr-core.
//! Re-export public storage API while keeping schema, records and layout SQL
//! separate from each other and the GPUI frontend.

use rusqlite::Connection;
use std::{error::Error, fmt};
use ukk_domain::LayoutError;

mod layouts;
mod records;
mod schema;
mod snapshot;
#[cfg(test)]
mod tests;

pub use schema::default_database_path;

#[derive(Debug)]
pub enum StoreError {
    Sql(rusqlite::Error),
    Json(serde_json::Error),
    Io(std::io::Error),
    Layout(LayoutError),
    Invalid(String),
    NotFound(String),
    Conflict,
    UnsupportedVersion(i64),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sql(e) => write!(f, "SQLite: {e}"),
            Self::Json(e) => write!(f, "layout JSON: {e}"),
            Self::Io(e) => write!(f, "filesystem: {e}"),
            Self::Layout(e) => write!(f, "invalid pane layout: {e}"),
            Self::Invalid(s) => write!(f, "invalid input: {s}"),
            Self::NotFound(s) => write!(f, "not found: {s}"),
            Self::Conflict => write!(f, "layout revision conflict; reload before retrying"),
            Self::UnsupportedVersion(v) => {
                write!(f, "database schema version {v} is newer than supported")
            }
        }
    }
}

impl Error for StoreError {}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sql(e)
    }
}
impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}
impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<LayoutError> for StoreError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

pub struct Store {
    conn: Connection,
}
