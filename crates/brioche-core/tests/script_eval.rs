use std::{collections::BTreeMap, sync::Arc};

use brioche_core::{
    recipe::{
        Directory, DownloadRecipe, ProcessRecipe, ProcessTemplate, ProcessTemplateComponent,
        Recipe, Symlink, UnarchiveRecipe,
    },
    script::runtime::{JsRuntime, initialize_js_platform},
};

#[tokio::test]
async fn test_script_eval_basic() {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    context
        .write_file(
            "myproject/project.bri",
            r#"
                export default function () {
                    return {
                        briocheSerialize() {
                            return {
                                type: "create_file",
                                content: "hello world",
                                executable: false,
                            };
                        }
                    };
                }
            "#,
        )
        .await;

    let project_ref =
        brioche_test_support::load_project(&mut *brioche.write().await, &project_dir).await;

    let js_runtime = JsRuntime::new(&brioche, initialize_js_platform())
        .await
        .unwrap();
    let default_ref = js_runtime
        .get_recipe_export(project_ref, "default")
        .await
        .unwrap();
    let default = brioche
        .read()
        .await
        .recipes()
        .get_recipe(default_ref)
        .clone();

    assert_eq!(
        *default,
        Recipe::CreateFile {
            content: b"hello world".into(),
            executable: false,
            resources: None
        }
    );
}

#[tokio::test]
async fn test_script_eval_imports() {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    context
        .write_toml(
            "myworkspace/brioche_workspace.toml",
            &brioche_core::project::WorkspaceDefinition {
                members: vec!["./myproject".parse().unwrap(), "./foo".parse().unwrap()],
            },
        )
        .await;

    let project_dir = context.mkdir("myworkspace/myproject").await;

    context
        .write_file(
            "myworkspace/myproject/project.bri",
            indoc::indoc! {r#"
                export { foo as default } from "foo";
            "#},
        )
        .await;
    context
        .write_file(
            "myworkspace/foo/project.bri",
            indoc::indoc! {r#"
                export { foo } from "./bar.bri";
            "#},
        )
        .await;
    context
        .write_file(
            "myworkspace/foo/bar.bri",
            indoc::indoc! {r#"
                export { foo } from "./baz";
            "#},
        )
        .await;
    context
        .write_file(
            "myworkspace/foo/baz/index.bri",
            indoc::indoc! {r#"
                export function foo() {
                    return {
                        briocheSerialize() {
                            return {
                                type: "create_file",
                                content: "hello world",
                                executable: false,
                            };
                        }
                    };
                }
            "#},
        )
        .await;

    let project_ref =
        brioche_test_support::load_project(&mut *brioche.write().await, &project_dir).await;

    let js_runtime = JsRuntime::new(&brioche, initialize_js_platform())
        .await
        .unwrap();
    let default_ref = js_runtime
        .get_recipe_export(project_ref, "default")
        .await
        .unwrap();
    let default = brioche
        .read()
        .await
        .recipes()
        .get_recipe(default_ref)
        .clone();

    assert_eq!(
        *default,
        Recipe::CreateFile {
            content: b"hello world".into(),
            executable: false,
            resources: None
        }
    );
}

#[tokio::test]
async fn test_script_eval_deserialize_recipes() {
    let (brioche, context) = brioche_test_support::brioche_test().await;

    let project_dir = context.mkdir("myproject").await;

    context
        .write_file(
            "myproject/project.bri",
            r#"
                function utf8Encode(s: string): Uint8Array {
                    return globalThis.Deno.core.ops.op_brioche_utf8_encode(s);
                }
                function tickEncode(s: Uint8Array | string): string {
                    const bytes = typeof s === "string" ? utf8Encode(s) : s;
                    return globalThis.Deno.core.ops.op_brioche_tick_encode(bytes);
                }

                export default function () {
                    return {
                        briocheSerialize() {
                            return {
                                type: "create_directory",
                                entries: {
                                    // file: {
                                    //     type: "file",
                                    // },
                                    directory: {
                                        type: "directory",
                                        entries: {},
                                    },
                                    symlink: {
                                        type: "symlink",
                                        target: tickEncode('foo/bar`'),
                                    },
                                    download: {
                                        type: "download",
                                        url: "https://example.com",
                                        hash: {
                                            type: "sha256",
                                            value: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                                        },
                                    },
                                    unarchive: {
                                        type: "unarchive",
                                        file: {
                                            type: "create_file",
                                            content: "",
                                            executable: false,
                                        },
                                        archive: "tar",
                                        compression: "zstd",
                                    },
                                    process: {
                                        type: "process",
                                        command: {components: [{
                                            type: "literal",
                                            value: tickEncode("ab`"),
                                        }]},
                                        args: [
                                            {components: [{
                                                type: "input",
                                                recipe: {
                                                    type: "create_file",
                                                    content: "",
                                                    executable: false,
                                                },
                                            }]},
                                            {components: []},
                                        ],
                                        env: {
                                            [tickEncode("`test`")]: {
                                                components: [{
                                                    type: "literal",
                                                    value: "",
                                                }],
                                            },
                                            "paths": {
                                                components: [
                                                    {type: "output_path"},
                                                    {type: "resource_dir"},
                                                    {type: "input_resource_dirs"},
                                                    {type: "home_dir"},
                                                    {type: "work_dir"},
                                                    {type: "temp_dir"},
                                                    {type: "ca_certificate_bundle_path"},
                                                ],
                                            },
                                            "lazy": {
                                                components: [
                                                    {
                                                        type: "input",
                                                        recipe: {
                                                            type: "create_file",
                                                            content: "",
                                                            executable: false,
                                                        },
                                                    },
                                                    {
                                                        type: "input",
                                                        recipe: () => ({
                                                            type: "create_file",
                                                            content: "",
                                                            executable: false,
                                                        }),
                                                    },
                                                    {
                                                        type: "input",
                                                        recipe: (async () => ({
                                                            type: "create_file",
                                                            content: "",
                                                            executable: false,
                                                        }))(),
                                                    },
                                                    {
                                                        type: "input",
                                                        recipe: async () => ({
                                                            type: "create_file",
                                                            content: "",
                                                            executable: false,
                                                        }),
                                                    },
                                                    {
                                                        type: "input",
                                                        recipe: async () => ({
                                                            briocheSerialize: async () => ({
                                                                type: "create_file",
                                                                content: "",
                                                                executable: false,
                                                            }),
                                                        }),
                                                    },
                                                ],
                                            }
                                        },
                                        currentDir: {
                                            components: [{ type: "output_path" }],
                                        },
                                        dependencies: [{
                                            type: "create_directory",
                                            entries: {},
                                        }],
                                        workDir: {
                                            type: "create_directory",
                                            entries: {},
                                        },
                                        outputScaffold: {
                                            type: "create_directory",
                                            entries: {},
                                        },
                                        platform: "x86_64-linux",
                                        isUnsafe: false,
                                        networking: false,
                                    },
                                    // complete_process: {
                                    //     type: "complete_process",
                                    // },
                                    create_file: {
                                        type: "create_file",
                                        content: tickEncode("`"),
                                        executable: true,
                                        resources: {
                                            type: "create_directory",
                                            entries: {},
                                        },
                                    },
                                    create_directory: {
                                        type: "create_directory",
                                        entries: {
                                            foo: {
                                                type: "create_file",
                                                content: "abc",
                                                executable: true,
                                            }
                                        },
                                    },
                                    cast: {
                                        type: "cast",
                                        recipe: {
                                            type: "create_directory",
                                            entries: {},
                                        },
                                        to: "directory",
                                    },
                                    merge: {
                                        type: "merge",
                                        directories: [{
                                            type: "create_directory",
                                            entries: {},
                                        }],
                                    },
                                    peel: {
                                        type: "peel",
                                        directory: {
                                            type: "create_directory",
                                            entries: {},
                                        },
                                        depth: 2,
                                    },
                                    get: {
                                        type: "get",
                                        directory: {
                                            type: "create_directory",
                                            entries: {},
                                        },
                                        path: tickEncode("`foo/bar"),
                                    },
                                    insert: {
                                        type: "insert",
                                        directory: {
                                            type: "create_directory",
                                            entries: {},
                                        },
                                        path: tickEncode("`foo/bar"),
                                        recipe: {
                                            type: "create_file",
                                            content: "",
                                            executable: false,
                                        },
                                    },
                                    // glob: {
                                    //     type: "glob",
                                    // },
                                    set_permissions: {
                                        type: "set_permissions",
                                        file: {
                                            type: "create_file",
                                            content: "",
                                            executable: false,
                                        },
                                        executable: true,
                                    },
                                    // collect_references: {
                                    //     type: "collect_references",
                                    // },
                                    // attach_resources: {
                                    //     type: "attach_resources",
                                    // },
                                    // proxy: {
                                    //     type: "proxy",
                                    // },
                                    // sync: {
                                    //     type: "sync",
                                    // },
                                },
                            };
                        }
                    };
                }
            "#,
        )
        .await;

    let project_ref =
        brioche_test_support::load_project(&mut *brioche.write().await, &project_dir).await;

    let js_runtime = JsRuntime::new(&brioche, initialize_js_platform())
        .await
        .unwrap();
    let default_ref = js_runtime
        .get_recipe_export(project_ref, "default")
        .await
        .unwrap();

    let brioche = &mut *brioche.write().await;

    let default = brioche.recipes().get_recipe(default_ref).clone();

    let mut entries = BTreeMap::new();

    {
        let directory = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(Recipe::Directory(Directory::default())));
        entries.insert("directory".into(), directory);
    }
    {
        let symlink = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(Recipe::Symlink(Symlink {
                target: "foo/bar`".into(),
            })));
        entries.insert("symlink".into(), symlink);
    }
    {
        let download = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(Recipe::Download(DownloadRecipe {
                url: "https://example.com".parse().unwrap(),
                hash: brioche_core::hash::AnyHash::Sha256 {
                    value: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                        .parse()
                        .unwrap(),
                },
            })));
        entries.insert("download".into(), download);
    }
    {
        let file = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(Recipe::CreateFile {
                content: "".into(),
                executable: false,
                resources: None,
            }));
        let unarchive = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(Recipe::Unarchive(UnarchiveRecipe {
                file,
                archive: brioche_core::recipe::ArchiveFormat::Tar,
                compression: brioche_core::recipe::CompressionFormat::Zstd,
            })));
        entries.insert("unarchive".into(), unarchive);
    }
    {
        let arg_1_input = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(Recipe::CreateFile {
                content: "".into(),
                executable: false,
                resources: None,
            }));
        let dependency_1 = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(Recipe::CreateDirectory {
                entries: BTreeMap::default(),
            }));
        let work_dir = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(Recipe::CreateDirectory {
                entries: BTreeMap::default(),
            }));
        let output_scaffold =
            brioche
                .recipes_mut()
                .insert_recipe(Arc::new(Recipe::CreateDirectory {
                    entries: BTreeMap::default(),
                }));
        let env_lazy_input = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(Recipe::CreateFile {
                content: "".into(),
                executable: false,
                resources: None,
            }));
        let process = ProcessRecipe {
            command: ProcessTemplate {
                components: vec![ProcessTemplateComponent::Literal {
                    value: "ab`".into(),
                }],
            },
            args: vec![
                ProcessTemplate {
                    components: vec![ProcessTemplateComponent::Input {
                        recipe: arg_1_input,
                    }],
                },
                ProcessTemplate { components: vec![] },
            ],
            env: BTreeMap::from_iter([
                (
                    "`test`".into(),
                    ProcessTemplate {
                        components: vec![ProcessTemplateComponent::Literal { value: "".into() }],
                    },
                ),
                (
                    "paths".into(),
                    ProcessTemplate {
                        components: vec![
                            ProcessTemplateComponent::OutputPath,
                            ProcessTemplateComponent::ResourceDir,
                            ProcessTemplateComponent::InputResourceDirs,
                            ProcessTemplateComponent::HomeDir,
                            ProcessTemplateComponent::WorkDir,
                            ProcessTemplateComponent::TempDir,
                            ProcessTemplateComponent::CaCertificateBundlePath,
                        ],
                    },
                ),
                (
                    "lazy".into(),
                    ProcessTemplate {
                        components: vec![
                            ProcessTemplateComponent::Input {
                                recipe: env_lazy_input,
                            },
                            ProcessTemplateComponent::Input {
                                recipe: env_lazy_input,
                            },
                            ProcessTemplateComponent::Input {
                                recipe: env_lazy_input,
                            },
                            ProcessTemplateComponent::Input {
                                recipe: env_lazy_input,
                            },
                            ProcessTemplateComponent::Input {
                                recipe: env_lazy_input,
                            },
                        ],
                    },
                ),
            ]),
            current_dir: ProcessTemplate {
                components: vec![ProcessTemplateComponent::OutputPath],
            },
            dependencies: vec![dependency_1],
            work_dir,
            output_scaffold: Some(output_scaffold),
            platform: brioche_core::platform::Platform::X86_64Linux,
            is_unsafe: false,
            networking: false,
        };
        let process = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(Recipe::Process(process)));
        entries.insert("process".into(), process);
    }
    {
        let resources = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(Recipe::CreateDirectory {
                entries: BTreeMap::default(),
            }));
        let create_file = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(Recipe::CreateFile {
                content: "`".into(),
                executable: true,
                resources: Some(resources),
            }));
        entries.insert("create_file".into(), create_file);
    }
    {
        let foo = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(Recipe::CreateFile {
                content: "abc".into(),
                executable: true,
                resources: None,
            }));
        let create_directory =
            brioche
                .recipes_mut()
                .insert_recipe(Arc::new(Recipe::CreateDirectory {
                    entries: BTreeMap::from_iter([("foo".into(), foo)]),
                }));
        entries.insert("create_directory".into(), create_directory);
    }
    {
        let recipe = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(Recipe::CreateDirectory {
                entries: BTreeMap::default(),
            }));
        let cast = brioche.recipes_mut().insert_recipe(Arc::new(Recipe::Cast {
            recipe,
            to: brioche_core::recipe::ArtifactKind::Directory,
        }));
        entries.insert("cast".into(), cast);
    }
    {
        let directory = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(Recipe::CreateDirectory {
                entries: BTreeMap::default(),
            }));
        let merge = brioche.recipes_mut().insert_recipe(Arc::new(Recipe::Merge {
            directories: vec![directory],
        }));
        entries.insert("merge".into(), merge);
    }
    {
        let directory = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(Recipe::CreateDirectory {
                entries: BTreeMap::default(),
            }));
        let peel = brioche.recipes_mut().insert_recipe(Arc::new(Recipe::Peel {
            directory,
            depth: 2,
        }));
        entries.insert("peel".into(), peel);
    }
    {
        let directory = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(Recipe::CreateDirectory {
                entries: BTreeMap::default(),
            }));
        let get = brioche.recipes_mut().insert_recipe(Arc::new(Recipe::Get {
            directory,
            path: "`foo/bar".into(),
        }));
        entries.insert("get".into(), get);
    }
    {
        let directory = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(Recipe::CreateDirectory {
                entries: BTreeMap::default(),
            }));
        let recipe = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(Recipe::CreateFile {
                content: "".into(),
                executable: false,
                resources: None,
            }));
        let insert = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(Recipe::Insert {
                directory,
                path: "`foo/bar".into(),
                recipe: Some(recipe),
            }));
        entries.insert("insert".into(), insert);
    }
    {
        let file = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(Recipe::CreateFile {
                content: "".into(),
                executable: false,
                resources: None,
            }));
        let set_permissions =
            brioche
                .recipes_mut()
                .insert_recipe(Arc::new(Recipe::SetPermissions {
                    file,
                    executable: Some(true),
                }));
        entries.insert("set_permissions".into(), set_permissions);
    }

    assert_eq!(*default, Recipe::CreateDirectory { entries });
}
