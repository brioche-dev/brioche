use std::collections::HashSet;

use assert_matches::assert_matches;
use brioche_core::{
    BriocheState,
    path::RelativePath,
    project::{ModuleRef, ProjectIssue, StaticQuery},
    script::parse::ScriptParseError,
};

fn module_static_queries(brioche: &BriocheState, module_ref: ModuleRef) -> HashSet<StaticQuery> {
    brioche
        .projects()
        .module_statics(module_ref)
        .map(|(query, _)| query.query.clone())
        .collect()
}

#[tokio::test]
async fn test_project_load_static_brioche_include() {
    let (brioche, context) = brioche_test_support::brioche_test().await;
    let brioche = &mut *brioche.write().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                Brioche.includeFile("foo");

                export default function () {
                    return Brioche.includeDirectory("bar");
                }
            "#,
        )
        .await;

    let project_ref =
        brioche_test_support::load_project_without_resolving(brioche, &project_dir).await;

    let root_module = brioche.projects().root_module(project_ref).unwrap();
    let statics = module_static_queries(brioche, root_module);

    assert_eq!(
        statics,
        HashSet::from_iter([
            StaticQuery::IncludeFile(RelativePath::new("foo")),
            StaticQuery::IncludeDirectory(RelativePath::new("bar")),
        ]),
    );
}

#[tokio::test]
async fn test_project_load_static_brioche_include_template_simple() {
    let (brioche, context) = brioche_test_support::brioche_test().await;
    let brioche = &mut *brioche.write().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                Brioche.includeFile(`foo`);

                export default function () {
                    return Brioche.includeDirectory(`"bar"`);
                }
            "#,
        )
        .await;

    let project_ref =
        brioche_test_support::load_project_without_resolving(brioche, &project_dir).await;

    let root_module = brioche.projects().root_module(project_ref).unwrap();
    let statics = module_static_queries(brioche, root_module);

    assert_eq!(
        statics,
        HashSet::from_iter([
            StaticQuery::IncludeFile(RelativePath::new("foo")),
            StaticQuery::IncludeDirectory(RelativePath::new("\"bar\"")),
        ]),
    );
}

#[tokio::test]
async fn test_project_load_static_brioche_include_template_nested_literal() {
    let (brioche, context) = brioche_test_support::brioche_test().await;
    let brioche = &mut *brioche.write().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                Brioche.includeFile(`foo/${"bar"}/${`baz`}`);
            "#,
        )
        .await;

    let project_ref =
        brioche_test_support::load_project_without_resolving(brioche, &project_dir).await;

    let root_module = brioche.projects().root_module(project_ref).unwrap();
    let statics = module_static_queries(brioche, root_module);

    assert_eq!(
        statics,
        HashSet::from_iter([StaticQuery::IncludeFile(RelativePath::new("foo/bar/baz"))]),
    );
}

#[tokio::test]
async fn test_project_load_static_brioche_glob() {
    let (brioche, context) = brioche_test_support::brioche_test().await;
    let brioche = &mut *brioche.write().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export default function () {
                    return Brioche.glob("./foo", "bar/**/*.txt");
                }
            "#,
        )
        .await;

    let project_ref =
        brioche_test_support::load_project_without_resolving(brioche, &project_dir).await;

    let root_module = brioche.projects().root_module(project_ref).unwrap();
    let statics = module_static_queries(brioche, root_module);

    assert_eq!(
        statics,
        HashSet::from_iter([StaticQuery::Glob {
            patterns: vec!["./foo".to_string(), "bar/**/*.txt".to_string(),]
        }]),
    );
}

#[tokio::test]
async fn test_project_load_static_brioche_download() {
    let (brioche, context) = brioche_test_support::brioche_test().await;
    let brioche = &mut *brioche.write().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export default function () {
                    return Brioche.download("https://example.com");
                }
            "#,
        )
        .await;

    let project_ref =
        brioche_test_support::load_project_without_resolving(brioche, &project_dir).await;

    let root_module = brioche.projects().root_module(project_ref).unwrap();
    let statics = module_static_queries(brioche, root_module);

    assert_eq!(
        statics,
        HashSet::from_iter([StaticQuery::Download {
            url: "https://example.com".parse().unwrap(),
        }]),
    );
}

#[tokio::test]
async fn test_project_load_static_brioche_download_with_project_version() {
    let (brioche, context) = brioche_test_support::brioche_test().await;
    let brioche = &mut *brioche.write().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {
                    version: "1.0.0",
                }

                export default function () {
                    return Brioche.download(`https://example.com/v${project.version}/download.tar.gz`);
                }
            "#,
        )
        .await;

    let project_ref =
        brioche_test_support::load_project_without_resolving(brioche, &project_dir).await;

    let root_module = brioche.projects().root_module(project_ref).unwrap();
    let statics = module_static_queries(brioche, root_module);

    assert_eq!(
        statics,
        HashSet::from_iter([StaticQuery::Download {
            url: "https://example.com/v1.0.0/download.tar.gz"
                .parse()
                .unwrap(),
        }]),
    );
}

#[tokio::test]
async fn test_project_load_static_brioche_download_with_project_version_brackets() {
    let (brioche, context) = brioche_test_support::brioche_test().await;
    let brioche = &mut *brioche.write().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {
                    version: "1.0.0",
                }

                export default function () {
                    return Brioche.download(`https://example.com/v${project["version"]}/download.tar.gz`);
                }
            "#,
        )
        .await;

    let project_ref =
        brioche_test_support::load_project_without_resolving(brioche, &project_dir).await;

    let root_module = brioche.projects().root_module(project_ref).unwrap();
    let statics = module_static_queries(brioche, root_module);

    assert_eq!(
        statics,
        HashSet::from_iter([StaticQuery::Download {
            url: "https://example.com/v1.0.0/download.tar.gz"
                .parse()
                .unwrap(),
        }]),
    );
}

#[tokio::test]
async fn test_project_load_static_brioche_download_with_project_extras() {
    let (brioche, context) = brioche_test_support::brioche_test().await;
    let brioche = &mut *brioche.write().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                interface Project {
                    name?: string;
                    version?: string;
                    extra?: Record<string, unknown>;
                }

                export const project = ({
                    version: "1.0.0" satisfies string,
                    extra: {
                        ["domain"]: (`example.com` as string),
                    },
                }) satisfies Project as const;

                export default function () {
                    return Brioche.download(`https://${project.extra["domain"]}/v${project["version"]}/download.tar.gz`);
                }
            "#,
        )
        .await;

    let project_ref =
        brioche_test_support::load_project_without_resolving(brioche, &project_dir).await;

    let root_module = brioche.projects().root_module(project_ref).unwrap();
    let statics = module_static_queries(brioche, root_module);

    assert_eq!(
        statics,
        HashSet::from_iter([StaticQuery::Download {
            url: "https://example.com/v1.0.0/download.tar.gz"
                .parse()
                .unwrap(),
        }]),
    );
}

#[tokio::test]
async fn test_project_load_static_brioche_include_escape_error() {
    let (brioche, context) = brioche_test_support::brioche_test().await;
    let brioche = &mut *brioche.write().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                Brioche.includeFile("\"'\\foo'\"");
            "#,
        )
        .await;

    let _project_ref =
        brioche_test_support::load_project_without_resolving_ignoring_issues(brioche, &project_dir)
            .await;

    // Escape sequences are not currently supported
    let issues = brioche_test_support::get_all_issues(brioche);
    assert_matches!(
        &issues[..],
        [ProjectIssue::ScriptParseError {
            error: ScriptParseError::UnsupportedStaticExpression { .. },
            ..
        }]
    );
}

#[tokio::test]
async fn test_project_load_static_brioche_include_template_escape_error() {
    let (brioche, context) = brioche_test_support::brioche_test().await;
    let brioche = &mut *brioche.write().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r"
                Brioche.includeFile(`foo`);

                export default function () {
                    return Brioche.includeDirectory(`\$bar`);
                }
            ",
        )
        .await;

    let _project_ref =
        brioche_test_support::load_project_without_resolving_ignoring_issues(brioche, &project_dir)
            .await;

    // Escape sequences are not currently supported
    let issues = brioche_test_support::get_all_issues(brioche);
    assert_matches!(
        &issues[..],
        [ProjectIssue::ScriptParseError {
            error: ScriptParseError::UnsupportedStaticExpression { .. },
            ..
        }]
    );
}

#[tokio::test]
async fn test_project_load_static_brioche_include_invalid() {
    let (brioche, context) = brioche_test_support::brioche_test().await;
    let brioche = &mut *brioche.write().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r"
                const x = Brioche.includeFile(`${123}`);

                export default function () {
                    return x;
                }
            ",
        )
        .await;

    let _project_ref =
        brioche_test_support::load_project_without_resolving_ignoring_issues(brioche, &project_dir)
            .await;

    let issues = brioche_test_support::get_all_issues(brioche);
    assert_matches!(
        &issues[..],
        [ProjectIssue::ScriptParseError {
            error: ScriptParseError::UnsupportedStaticExpression { .. },
            ..
        }]
    );
}

#[tokio::test]
async fn test_project_load_static_brioche_download_with_project_version_cross_module_error() {
    let (brioche, context) = brioche_test_support::brioche_test().await;
    let brioche = &mut *brioche.write().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                import { foo } from "./foo.bri";

                export const project = {
                    version: "1.0.0",
                }

                export default function () {
                    return foo();
                    return Brioche.download(`https://example.com/v${project["version"]}/download.tar.gz`);
                }
            "#,
        )
        .await;
    context
        .write_file(
            "myproject/foo.bri",
            r#"
                // This is a different variable called "project", not the
                // actual project export
                export const project = {
                    version: "x",
                }

                export default function foo() {
                    return Brioche.download(`https://example.com/v${project.version}/download.tar.gz`);
                }
            "#,
        )
        .await;

    let _project_ref =
        brioche_test_support::load_project_without_resolving_ignoring_issues(brioche, &project_dir)
            .await;

    // Only statics in the root module can access the `project` variable
    let issues = brioche_test_support::get_all_issues(brioche);
    assert_matches!(
        &issues[..],
        [ProjectIssue::ScriptParseError {
            error: ScriptParseError::UnsupportedStaticExpression { .. },
            ..
        }]
    );
}
