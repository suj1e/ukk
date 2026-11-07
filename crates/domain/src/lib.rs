//! Portable ukk product domain; independent of GPUI, Host, SQLite and hc.
mod entities;
mod ids;
mod panes;

pub use entities::{AgentRun, DomainSnapshot, Session, Space};
pub use ids::new_id;
pub use panes::{LayoutError, PaneContent, PaneNode, SessionLayout, SplitAxis};
