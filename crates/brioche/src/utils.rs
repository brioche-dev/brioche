use std::collections::HashSet;
use std::path::PathBuf;

/// Resolves input paths by merging positional args with the deprecated `--project` flag,
/// then deduplicates them. Only project directories are accepted — individual files are rejected.
///
/// Returns the current directory as a default when no paths are provided.
pub fn resolve_project_paths(
    projects: Vec<PathBuf>,
    project: Vec<PathBuf>,
) -> anyhow::Result<HashSet<PathBuf>> {
    if projects.is_empty() && project.is_empty() {
        let path = PathBuf::from(".");
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        return Ok(HashSet::from([path]));
    }

    let all_paths = projects.into_iter().chain(project);
    let mut result = HashSet::with_capacity(all_paths.size_hint().0);

    for path in all_paths {
        if path.is_file() {
            anyhow::bail!(
                "path '{}' is a file, expected a project directory",
                path.display()
            );
        }
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        result.insert(path);
    }

    Ok(result)
}

/// Records the outcome of a per-project operation into a shared error flag.
///
/// - On `Err`, emits a red error message to the reporter and sets `error_result` to `Some(())`.
/// - On `Ok(false)` (operation completed but reported failure), sets `error_result` without
///   emitting an additional message.
/// - On `Ok(true)` (success), does nothing.
pub fn consolidate_result(
    reporter: &brioche_core::reporter::Reporter,
    project_name: Option<&str>,
    result: Result<bool, anyhow::Error>,
    error_result: &mut Option<()>,
) {
    match result {
        Err(err) => {
            let format_string = project_name.map_or_else(
                || format!("Error occurred in project: {err}"),
                |project_name| format!("Error occurred with {project_name}: {err}"),
            );

            reporter.emit(superconsole::Lines::from_multiline_string(
                &format_string,
                superconsole::style::ContentStyle {
                    foreground_color: Some(superconsole::style::Color::Red),
                    ..superconsole::style::ContentStyle::default()
                },
            ));

            *error_result = Some(());
        }
        Ok(false) => {
            *error_result = Some(());
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_inputs_defaults_to_current_dir() {
        let result = resolve_project_paths(vec![], vec![]).unwrap();
        let expected = std::fs::canonicalize(".").unwrap();
        assert_eq!(result, HashSet::from([expected]));
    }

    #[test]
    fn test_deduplicates_paths() {
        let dir = std::env::temp_dir().join("brioche_utils_dedup_test");
        std::fs::create_dir_all(&dir).unwrap();

        let path_a = dir.join("./");
        let path_b = dir.clone();
        let result = resolve_project_paths(vec![path_a, path_b], vec![]).unwrap();
        assert_eq!(result.len(), 1);

        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn test_nonexistent_path_accepted() {
        let path = PathBuf::from("this/path/does/not/exist");
        let result = resolve_project_paths(vec![path.clone()], vec![]);
        assert!(result.is_ok());
        assert!(result.unwrap().contains(&path));
    }
}
