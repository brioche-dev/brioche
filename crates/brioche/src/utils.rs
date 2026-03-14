use std::collections::HashSet;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

const DEFAULT_EXPORT: &str = "default";

/// Resolves input paths by merging positional args with the deprecated `--project` flag,
/// then deduplicates them. Only project directories are accepted -- individual files are rejected.
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
                || format!("Error: {err:#}"),
                |project_name| format!("Error occurred with {project_name}: {err:#}"),
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

/// A parsed reference to a project and its exports.
///
/// The syntax is `project^export1,export2` where:
/// - The project part is a local path (starting with `./`, `../`, `/`) or a registry name.
/// - The exports are comma-separated. If omitted, defaults to `DEFAULT_EXPORT`.
/// - A bare `^export` means the current directory with the given export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRefs {
    pub source: ProjectSource,
    pub exports: Vec<String>,
}

#[derive(Debug, PartialEq, Eq, Hash)]
pub struct ProjectRef {
    pub source: ProjectSource,
    pub export: String,
}

/// Where a project comes from: a local path or a registry name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProjectSource {
    Local(PathBuf),
    Registry(String),
}

#[derive(Clone)]
pub struct ProjectRefsParser;

impl clap::builder::TypedValueParser for ProjectRefsParser {
    type Value = ProjectRefs;

    fn parse_ref(
        &self,
        cmd: &clap::Command,
        _arg: Option<&clap::Arg>,
        value: &OsStr,
    ) -> Result<Self::Value, clap::Error> {
        let s = value.to_str().ok_or_else(|| {
            cmd.clone()
                .error(clap::error::ErrorKind::InvalidUtf8, "invalid UTF-8")
        })?;

        let (project_part, exports) = if let Some((project, exports)) = s.split_once('^') {
            let exports: Vec<String> = exports.split(',').map(String::from).collect();
            if exports.iter().any(String::is_empty) {
                return Err(cmd.clone().error(
                    clap::error::ErrorKind::ValueValidation,
                    format!("empty export name in '{s}'"),
                ));
            }
            (project, exports)
        } else {
            (s, vec![DEFAULT_EXPORT.to_string()])
        };

        // Resolve the project source based on the project part
        // First check if it's a local path, then a registry name
        let source = if project_part.is_empty() {
            ProjectSource::Local(PathBuf::from("."))
        } else if project_part.starts_with("./")
            || project_part.starts_with("../")
            || project_part.starts_with('/')
        {
            ProjectSource::Local(PathBuf::from(project_part))
        } else if project_part.contains('/') {
            return Err(cmd.clone().error(
                clap::error::ErrorKind::ValueValidation,
                format!("paths must start with './' or '../'. Did you mean \"./{project_part}\"?"),
            ));
        } else {
            ProjectSource::Registry(project_part.to_string())
        };

        Ok(ProjectRefs { source, exports })
    }
}

impl fmt::Display for ProjectSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(project) => write!(formatter, "project '{}'", project.display()),
            Self::Registry(registry) => write!(formatter, "registry project '{registry}'"),
        }
    }
}

impl fmt::Display for ProjectRefs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.source)?;
        if !self.exports.is_empty() {
            write!(formatter, "^{}", self.exports.join(","))?;
        }
        Ok(())
    }
}

/// Resolves build targets from positional args and flags.
pub fn resolve_project_refs(
    positional: Vec<ProjectRefs>,
    project: Option<PathBuf>,
    registry: Option<String>,
    export: Option<String>,
) -> HashSet<ProjectRef> {
    let project_refs = if positional.is_empty() {
        let export = export.unwrap_or_else(|| DEFAULT_EXPORT.to_string());
        let exports = vec![export];

        let source = match (project, registry) {
            (Some(project), None) => ProjectSource::Local(project),
            (None, Some(registry)) => ProjectSource::Registry(registry),
            (None, None) => ProjectSource::Local(PathBuf::from(".")),
            (Some(_), Some(_)) => unreachable!("clap prevents --project and --registry together"),
        };

        vec![ProjectRefs { source, exports }]
    } else {
        positional
    };

    project_refs
        .into_iter()
        .flat_map(|target| {
            target.exports.into_iter().map(move |export| ProjectRef {
                source: target.source.clone(),
                export,
            })
        })
        .collect()
}

/// Generate numbered output paths from a base path.
///
/// - `count == 0`: `[]`
/// - `count > 1`: `[base, base-1, base-2]`
pub fn numbered_output_paths(base: &Path, count: usize) -> Vec<PathBuf> {
    if count == 0 {
        return vec![];
    }

    let mut paths = Vec::with_capacity(count);
    paths.push(base.to_path_buf());

    let stem = base
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = base.parent();

    for i in 1..count {
        let suffixed = format!("{stem}-{i}");
        let path = parent.map_or_else(|| PathBuf::from(&suffixed), |path| path.join(&suffixed));
        paths.push(path);
    }

    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    use clap::builder::TypedValueParser as _;

    fn parse_project_refs(s: &str) -> Result<ProjectRefs, clap::Error> {
        let cmd = clap::Command::new("test");
        ProjectRefsParser.parse_ref(&cmd, None, OsStr::new(s))
    }

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

    #[test]
    fn test_project_ref_local_path_default_export() {
        let project_refs = parse_project_refs("./packages/curl").unwrap();
        assert_eq!(
            project_refs.source,
            ProjectSource::Local(PathBuf::from("./packages/curl"))
        );
        assert_eq!(project_refs.exports, vec![DEFAULT_EXPORT]);
    }

    #[test]
    fn test_project_ref_registry_default_export() {
        let project_refs = parse_project_refs("curl").unwrap();
        assert_eq!(
            project_refs.source,
            ProjectSource::Registry("curl".to_string())
        );
        assert_eq!(project_refs.exports, vec![DEFAULT_EXPORT]);
    }

    #[test]
    fn test_project_ref_local_path_single_export() {
        let project_refs = parse_project_refs("./packages/curl^test").unwrap();
        assert_eq!(
            project_refs.source,
            ProjectSource::Local(PathBuf::from("./packages/curl"))
        );
        assert_eq!(project_refs.exports, vec!["test"]);
    }

    #[test]
    fn test_project_ref_registry_multiple_exports() {
        let project_refs = parse_project_refs("curl^test,default").unwrap();
        assert_eq!(
            project_refs.source,
            ProjectSource::Registry("curl".to_string())
        );
        assert_eq!(project_refs.exports, vec!["test", DEFAULT_EXPORT]);
    }

    #[test]
    fn test_project_ref_bare_caret_current_dir() {
        let project_refs = parse_project_refs("^test").unwrap();
        assert_eq!(
            project_refs.source,
            ProjectSource::Local(PathBuf::from("."))
        );
        assert_eq!(project_refs.exports, vec!["test"]);
    }

    #[test]
    fn test_resolve_project_refs_default() {
        let project_refs = resolve_project_refs(vec![], None, None, None);
        let result = project_refs.iter().collect::<Vec<_>>();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source, ProjectSource::Local(PathBuf::from(".")));
        assert_eq!(result[0].export, DEFAULT_EXPORT);
    }
}
