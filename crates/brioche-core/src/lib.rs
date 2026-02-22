use std::sync::Arc;

use tokio::sync::RwLock;

mod hash;
pub mod path;
pub mod projects;
mod script;

#[derive(Default, Clone)]
pub struct Brioche {
    projects: Arc<RwLock<projects::Projects>>,
}
