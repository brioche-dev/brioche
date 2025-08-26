use std::collections::{BTreeSet, HashMap};

use assert_matches::assert_matches;
use brioche_core::{
    project::{
        DependencyDefinition, ProjectDefinition, Version,
        analyze::{ImportAnalysis, ProjectAnalysis, StaticInclude, StaticQuery, analyze_project},
    },
    script::specifier::{BriocheImportSpecifier, BriocheModuleSpecifier},
};

fn get_local_module(
    project: &ProjectAnalysis,
    referrer: &BriocheModuleSpecifier,
    specifier: &str,
) -> BriocheModuleSpecifier {
    let specifier: BriocheImportSpecifier = specifier.parse().expect("invalid import specifier");
    let Some(referrer_module) = project.local_modules.get(referrer) else {
        panic!("referrer {referrer} not found as a local module");
    };
    let Some(module) = referrer_module.imports.get(&specifier) else {
        panic!("module {specifier} not found as an import in {referrer}");
    };

    match module {
        ImportAnalysis::LocalModule(module) => module.clone(),
        ImportAnalysis::ExternalProject(_) => panic!("module {specifier} is an external module"),
    }
}

#[tokio::test]
async fn test_analyze_simple_project() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r"
                export const project = {};
            ",
        )
        .await;

    let project = analyze_project(&brioche.vfs, &project_dir).await?;

    assert_eq!(
        project.definition,
        ProjectDefinition {
            name: None,
            version: None,
            dependencies: HashMap::new(),
        },
    );

    let root_module = &project.local_modules[&project.root_module];
    assert!(root_module.imports.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_analyze_project_metadata() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {
                    name: "myproject",
                    version: "0.1.0",
                };
            "#,
        )
        .await;

    let project = analyze_project(&brioche.vfs, &project_dir).await?;

    assert_eq!(
        project.definition,
        ProjectDefinition {
            name: Some("myproject".to_string()),
            version: Some("0.1.0".to_string()),
            dependencies: HashMap::new(),
        },
    );

    let root_module = &project.local_modules[&project.root_module];
    assert!(root_module.imports.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_analyze_imports() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                import foo from "./foo.bri";
                import * from "/bar.bri";
                export { baz } from "./baz";
                export * from "/qux";
                export * as asdf from "./asdf.bri";

                export const project = {};
            "#,
        )
        .await;
    context
        .write_file("myproject/foo.bri", "export default 'foo';")
        .await;
    context
        .write_file("myproject/bar.bri", "export const bar = 'bar';")
        .await;
    context
        .write_file("myproject/baz/index.bri", "export const baz = 'baz';")
        .await;
    context
        .write_file("myproject/qux/index.bri", "export const qux = 'qux';")
        .await;
    context
        .write_file("myproject/asdf.bri", "export const asdf = 'asdf';")
        .await;

    let project = analyze_project(&brioche.vfs, &project_dir).await?;

    let root_module = &project.local_modules[&project.root_module];
    assert_matches!(
        root_module.imports[&"./foo.bri".parse().unwrap()],
        ImportAnalysis::LocalModule(_)
    );
    assert_matches!(
        root_module.imports[&"/bar.bri".parse().unwrap()],
        ImportAnalysis::LocalModule(_)
    );
    assert_matches!(
        root_module.imports[&"./baz".parse().unwrap()],
        ImportAnalysis::LocalModule(_)
    );
    assert_matches!(
        root_module.imports[&"/qux".parse().unwrap()],
        ImportAnalysis::LocalModule(_)
    );
    assert_matches!(
        root_module.imports[&"./asdf.bri".parse().unwrap()],
        ImportAnalysis::LocalModule(_)
    );

    Ok(())
}

#[tokio::test]
async fn test_analyze_nested_imports() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export { x } from "./foo";

                export const project = {};
            "#,
        )
        .await;
    context
        .write_file("myproject/foo/index.bri", "export { x } from '../bar';")
        .await;
    context
        .write_file("myproject/bar/index.bri", "export { x } from '/baz';")
        .await;
    context
        .write_file("myproject/baz/index.bri", "export const x = 'baz';")
        .await;

    let project = analyze_project(&brioche.vfs, &project_dir).await?;

    let foo_module = get_local_module(&project, &project.root_module, "./foo");
    let bar_module = get_local_module(&project, &foo_module, "../bar");
    let baz_module = get_local_module(&project, &bar_module, "/baz");

    assert!(project.local_modules[&baz_module].imports.is_empty());

    Ok(())
}

#[tokio::test]
async fn test_analyze_import_loop() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                import "./foo.bri";
                import "./bar.bri";

                export const project = {};
            "#,
        )
        .await;
    context
        .write_file(
            "myproject/foo.bri",
            r#"
                import "./bar.bri";
            "#,
        )
        .await;
    context
        .write_file(
            "myproject/bar.bri",
            r#"
                import "./foo.bri";
            "#,
        )
        .await;

    let project = analyze_project(&brioche.vfs, &project_dir).await?;

    let foo_module_from_root = get_local_module(&project, &project.root_module, "./foo.bri");
    let bar_module_from_root = get_local_module(&project, &project.root_module, "./bar.bri");
    let bar_module_from_foo = get_local_module(&project, &foo_module_from_root, "./bar.bri");
    let foo_module_from_bar = get_local_module(&project, &bar_module_from_root, "./foo.bri");

    assert_eq!(foo_module_from_root, foo_module_from_bar);
    assert_eq!(bar_module_from_root, bar_module_from_foo);

    Ok(())
}

#[tokio::test]
async fn test_analyze_external_dep() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                import "foo";

                export const project = {
                    dependencies: {
                        foo: "*",
                    },
                };
            "#,
        )
        .await;

    let project = analyze_project(&brioche.vfs, &project_dir).await?;

    assert_eq!(
        project.definition,
        ProjectDefinition {
            name: None,
            version: None,
            dependencies: HashMap::from_iter([(
                "foo".to_string(),
                DependencyDefinition::Version(Version::Any),
            ),]),
        }
    );

    let root_module = &project.local_modules[&project.root_module];
    let foo_module = &root_module.imports[&"foo".parse().unwrap()];

    assert_eq!(
        foo_module,
        &ImportAnalysis::ExternalProject("foo".to_string())
    );

    Ok(())
}

#[tokio::test]
async fn test_analyze_static_brioche_include() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                Brioche.includeFile("foo");

                export function () {
                    return Brioche.includeDirectory("bar");
                }
            "#,
        )
        .await;

    let project = analyze_project(&brioche.vfs, &project_dir).await?;

    let root_module = &project.local_modules[&project.root_module];

    assert_eq!(
        root_module.statics,
        BTreeSet::from_iter([
            StaticQuery::Include(StaticInclude::File {
                path: "foo".to_string()
            }),
            StaticQuery::Include(StaticInclude::Directory {
                path: "bar".to_string()
            }),
        ]),
    );

    Ok(())
}

#[tokio::test]
async fn test_analyze_static_brioche_include_escape_error() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                Brioche.includeFile("\"'\\foo'\"");
            "#,
        )
        .await;

    let result = analyze_project(&brioche.vfs, &project_dir).await;

    // Escape sequences are not currently supported
    assert_matches!(result, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_analyze_static_brioche_include_template_simple() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                Brioche.includeFile(`foo`);

                export function () {
                    return Brioche.includeDirectory(`"bar"`);
                }
            "#,
        )
        .await;

    let project = analyze_project(&brioche.vfs, &project_dir).await?;

    let root_module = &project.local_modules[&project.root_module];

    assert_eq!(
        root_module.statics,
        BTreeSet::from_iter([
            StaticQuery::Include(StaticInclude::File {
                path: "foo".to_string()
            }),
            StaticQuery::Include(StaticInclude::Directory {
                path: "\"bar\"".to_string()
            }),
        ]),
    );

    Ok(())
}

#[tokio::test]
async fn test_analyze_static_brioche_include_template_nested_literal() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                Brioche.includeFile(`foo/${"bar"}/${`baz`}`);
            "#,
        )
        .await;

    let project = analyze_project(&brioche.vfs, &project_dir).await?;

    let root_module = &project.local_modules[&project.root_module];

    assert_eq!(
        root_module.statics,
        BTreeSet::from_iter([StaticQuery::Include(StaticInclude::File {
            path: "foo/bar/baz".to_string()
        }),]),
    );

    Ok(())
}

#[tokio::test]
async fn test_analyze_static_brioche_include_template_escape_error() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r"
                Brioche.includeFile(`foo`);

                export function () {
                    return Brioche.includeDirectory(`\$bar`);
                }
            ",
        )
        .await;

    let result = analyze_project(&brioche.vfs, &project_dir).await;

    // Escape sequences are not currently supported
    assert_matches!(result, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_analyze_static_brioche_include_invalid() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r"
                const x = Brioche.includeFile(`${123}`);

                export function () {
                    return x;
                }
            ",
        )
        .await;

    let result = analyze_project(&brioche.vfs, &project_dir).await;
    assert_matches!(result, Err(_));

    Ok(())
}

#[tokio::test]
async fn test_analyze_static_brioche_glob() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export function () {
                    return Brioche.glob("./foo", "bar/**/*.txt");
                }
            "#,
        )
        .await;

    let project = analyze_project(&brioche.vfs, &project_dir).await?;

    let root_module = &project.local_modules[&project.root_module];

    assert_eq!(
        root_module.statics,
        BTreeSet::from_iter([StaticQuery::Glob {
            patterns: vec!["./foo".to_string(), "bar/**/*.txt".to_string(),]
        }]),
    );

    Ok(())
}

#[tokio::test]
async fn test_analyze_static_brioche_download() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export function () {
                    return Brioche.download("https://example.com");
                }
            "#,
        )
        .await;

    let project = analyze_project(&brioche.vfs, &project_dir).await?;

    let root_module = &project.local_modules[&project.root_module];

    assert_eq!(
        root_module.statics,
        BTreeSet::from_iter([StaticQuery::Download {
            url: "https://example.com".parse()?,
        }]),
    );

    Ok(())
}

#[tokio::test]
async fn test_analyze_static_brioche_download_with_project_version() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {
                    version: "1.0.0",
                }

                export function () {
                    return Brioche.download(`https://example.com/v${project.version}/download.tar.gz`);
                }
            "#,
        )
        .await;

    let project = analyze_project(&brioche.vfs, &project_dir).await?;

    let root_module = &project.local_modules[&project.root_module];

    assert_eq!(
        root_module.statics,
        BTreeSet::from_iter([StaticQuery::Download {
            url: "https://example.com/v1.0.0/download.tar.gz".parse()?,
        }]),
    );

    Ok(())
}

#[tokio::test]
async fn test_analyze_static_brioche_download_with_project_version_brackets() -> anyhow::Result<()>
{
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                export const project = {
                    version: "1.0.0",
                }

                export function () {
                    return Brioche.download(`https://example.com/v${project["version"]}/download.tar.gz`);
                }
            "#,
        )
        .await;

    let project = analyze_project(&brioche.vfs, &project_dir).await?;

    let root_module = &project.local_modules[&project.root_module];

    assert_eq!(
        root_module.statics,
        BTreeSet::from_iter([StaticQuery::Download {
            url: "https://example.com/v1.0.0/download.tar.gz".parse()?,
        }]),
    );

    Ok(())
}

#[tokio::test]
async fn test_analyze_static_brioche_download_with_project_extras() -> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

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

                export function () {
                    return Brioche.download(`https://${project.extra["domain"]}/v${project["version"]}/download.tar.gz`);
                }
            "#,
        )
        .await;

    let project = analyze_project(&brioche.vfs, &project_dir).await?;

    let root_module = &project.local_modules[&project.root_module];

    assert_eq!(
        root_module.statics,
        BTreeSet::from_iter([StaticQuery::Download {
            url: "https://example.com/v1.0.0/download.tar.gz".parse()?,
        }]),
    );

    Ok(())
}

#[tokio::test]
async fn test_analyze_static_brioche_download_with_project_version_cross_module_error()
-> anyhow::Result<()> {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;
    context
        .write_file(
            "myproject/project.bri",
            r#"
                import { foo } from "./foo.bri";

                export const project = {
                    version: "1.0.0",
                }

                export function () {
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

                export function foo() {
                    return Brioche.download(`https://example.com/v${project.version}/download.tar.gz`);
                }
            "#,
        )
        .await;

    let result = analyze_project(&brioche.vfs, &project_dir).await;

    // Only statics in the root module can access the `project` variable
    assert_matches!(result, Err(_));

    Ok(())
}
