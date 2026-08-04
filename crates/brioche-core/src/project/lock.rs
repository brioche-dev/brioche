use std::collections::HashSet;

use crate::{
    BriocheMut,
    project::{LockfileState, ProjectRef},
};

pub async fn commit_all_dirty_lockfiles(
    brioche: &mut BriocheMut<'_>,
) -> Result<HashSet<ProjectRef>, CommitLockfileError> {
    let projects = &mut brioche.state.projects;
    let mut updated_projects = HashSet::new();

    for (project_ref, project) in &mut projects.projects {
        match &mut project.lockfile_state {
            LockfileState::Clean(_) => {}
            LockfileState::Dirty { old: _, new } => {
                let local_project_path = &projects.local_project_paths[project_ref];
                let temp_lockfile_path =
                    local_project_path.join_one(format!("_brioche.lock.{}", ulid::Ulid::new()));
                let lockfile_path = local_project_path.join_one("brioche.lock");

                let temp_lockfile_path = temp_lockfile_path.to_system_path()?;
                let lockfile_path = lockfile_path.to_system_path()?;

                let lockfile_content =
                    serde_json::to_string_pretty(new).expect("failed to serialize lockfile");
                tokio::fs::write(&temp_lockfile_path, &lockfile_content).await?;
                tokio::fs::rename(&temp_lockfile_path, &lockfile_path).await?;

                project.lockfile_state = LockfileState::Clean(std::mem::take(new));
                updated_projects.insert(*project_ref);
            }
        }
    }

    Ok(updated_projects)
}

#[derive(Debug, thiserror::Error)]
pub enum CommitLockfileError {
    #[error(transparent)]
    IoError(#[from] std::io::Error),
    #[error(transparent)]
    ToSystemPathError(#[from] crate::path::ToSystemPathError),
}
