use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

use bstr::BString;
use joinery::JoinableIterator as _;
use petgraph::stable_graph::NodeIndex;

use crate::{
    blob::BlobHash,
    path::AbsolutePath,
    project::ModuleRef,
    recipe::{ArchiveFormat, ArtifactKind, CompressionFormat, Recipe, RecipeKind, RecipeRef},
    script::runtime::JsRuntimeError,
};

#[derive(Default)]
pub struct EvalRecipeState {
    graph: petgraph::stable_graph::StableGraph<EvalRecipeNode, EvalRecipeEdge>,
    recipes_by_value: HashMap<deno_core::v8::Global<deno_core::v8::Value>, EvalRecipeRef>,
    recipe_to_finished_recipe: HashMap<EvalRecipeRef, RecipeRef>,
    function_outputs: HashMap<
        deno_core::v8::Global<deno_core::v8::Function>,
        deno_core::v8::Global<deno_core::v8::Value>,
    >,
}

fn eval_recipes_tick(
    brioche: &mut crate::BriocheState,
    js_scope: &mut deno_core::v8::PinScope,
    state: &mut EvalRecipeState,
) -> Result<bool, DeserializeError> {
    let pending_recipes = state
        .graph
        .node_indices()
        .filter_map(|index| match &state.graph[index] {
            EvalRecipeNode::Recipe { .. } => None,
            EvalRecipeNode::PendingRecipe { value, scope } => {
                Some((EvalRecipeRef(index), value.clone(), scope.clone()))
            }
        })
        .collect::<Vec<_>>();

    let mut did_resolve_any = false;
    for (recipe_ref, promise, scope) in pending_recipes {
        let promise = deno_core::v8::Local::new(js_scope, promise);
        let resolved = match promise.state() {
            deno_core::v8::PromiseState::Pending => {
                continue;
            }
            deno_core::v8::PromiseState::Rejected => {
                let rejection = promise.result(js_scope);
                let message = get_rejection_message(js_scope, rejection);
                return Err(DeserializeError::new(
                    JsRuntimeError::PromiseRejected { message },
                    scope,
                ));
            }
            deno_core::v8::PromiseState::Fulfilled => promise.result(js_scope),
        };

        did_resolve_any = true;

        deserialize_partial_recipe_to(
            brioche,
            js_scope,
            state,
            JsValue {
                value: deno_core::v8::Global::new(js_scope, resolved),
                scope,
            },
            Some(recipe_ref),
        )?;
    }

    Ok(did_resolve_any)
}

#[derive(Clone)]
enum EvalRecipeNode {
    Recipe {
        recipe: Arc<EvalRecipe>,
        scope: JsValueScope,
    },
    PendingRecipe {
        value: deno_core::v8::Global<deno_core::v8::Promise>,
        scope: JsValueScope,
    },
}

impl EvalRecipeNode {
    const fn scope(&self) -> &JsValueScope {
        match self {
            Self::Recipe { scope, .. } | Self::PendingRecipe { scope, .. } => scope,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct EvalRecipeEdge;

enum EvalRecipe {
    File {
        content_blob: BlobHash,
        executable: bool,
        resources: Option<EvalRecipeRef>,
    },
    Directory {
        entries: BTreeMap<BString, EvalRecipeRef>,
    },
    Symlink(crate::recipe::Symlink),
    Download(crate::recipe::DownloadRecipe),
    Unarchive {
        file: EvalRecipeRef,
        archive: ArchiveFormat,
        compression: CompressionFormat,
    },
    Process {
        command: EvalProcessTemplate,
        args: Vec<EvalProcessTemplate>,
        env: BTreeMap<BString, EvalProcessTemplate>,
        current_dir: EvalProcessTemplate,
        dependencies: Vec<EvalRecipeRef>,
        work_dir: EvalRecipeRef,
        output_scaffold: Option<EvalRecipeRef>,
        platform: crate::platform::Platform,
        is_unsafe: bool,
        networking: bool,
    },
    CreateFile {
        content: BString,
        executable: bool,
        resources: Option<EvalRecipeRef>,
    },
    CreateDirectory {
        entries: BTreeMap<BString, EvalRecipeRef>,
    },
    Cast {
        recipe: EvalRecipeRef,
        to: ArtifactKind,
    },
    Merge {
        directories: Vec<EvalRecipeRef>,
    },
    Peel {
        directory: EvalRecipeRef,
        depth: u32,
    },
    Get {
        directory: EvalRecipeRef,
        path: BString,
    },
    Insert {
        directory: EvalRecipeRef,
        path: BString,
        recipe: Option<EvalRecipeRef>,
    },
    Glob {
        directory: EvalRecipeRef,
        patterns: BTreeSet<BString>,
    },
    SetPermissions {
        file: EvalRecipeRef,
        executable: Option<bool>,
    },
    CollectReferences {
        recipe: EvalRecipeRef,
    },
    AttachResources {
        recipe: EvalRecipeRef,
    },
    Proxy {
        recipe: EvalRecipeRef,
    },
    Sync {
        recipe: EvalRecipeRef,
    },
}

impl EvalRecipe {
    fn push_recipe_refs(&self, recipe_refs: &mut Vec<EvalRecipeRef>) {
        match self {
            Self::Symlink(_) | Self::Download(_) => {}
            Self::Unarchive { file, .. }
            | Self::SetPermissions {
                file,
                executable: _,
            } => {
                recipe_refs.push(*file);
            }
            Self::Process {
                command,
                args,
                env,
                current_dir,
                dependencies,
                work_dir,
                output_scaffold,
                platform: _,
                is_unsafe: _,
                networking: _,
            } => {
                command.push_recipe_refs(recipe_refs);
                for arg in args {
                    arg.push_recipe_refs(recipe_refs);
                }
                for env_value in env.values() {
                    env_value.push_recipe_refs(recipe_refs);
                }
                current_dir.push_recipe_refs(recipe_refs);
                recipe_refs.extend_from_slice(dependencies);
                recipe_refs.push(*work_dir);
                recipe_refs.extend(*output_scaffold);
            }
            Self::File { resources, .. }
            | Self::CreateFile {
                content: _,
                executable: _,
                resources,
            } => {
                recipe_refs.extend(resources);
            }
            Self::Directory { entries } | Self::CreateDirectory { entries } => {
                recipe_refs.extend(entries.values().copied());
            }
            Self::Cast { recipe, to: _ }
            | Self::CollectReferences { recipe }
            | Self::AttachResources { recipe }
            | Self::Proxy { recipe }
            | Self::Sync { recipe } => {
                recipe_refs.push(*recipe);
            }
            Self::Merge { directories } => {
                recipe_refs.extend_from_slice(directories);
            }
            Self::Peel {
                directory,
                depth: _,
            }
            | Self::Get { directory, path: _ }
            | Self::Glob {
                directory,
                patterns: _,
            } => {
                recipe_refs.push(*directory);
            }
            Self::Insert {
                directory,
                path: _,
                recipe,
            } => {
                recipe_refs.push(*directory);
                recipe_refs.extend(*recipe);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct EvalRecipeRef(NodeIndex);

struct EvalProcessTemplate {
    components: Vec<EvalProcessTemplateComponent>,
}

impl EvalProcessTemplate {
    fn default_current_dir() -> Self {
        Self {
            components: vec![EvalProcessTemplateComponent::WorkDir],
        }
    }

    fn push_recipe_refs(&self, recipe_refs: &mut Vec<EvalRecipeRef>) {
        for component in &self.components {
            component.push_recipe_refs(recipe_refs);
        }
    }
}

enum EvalProcessTemplateComponent {
    Literal { value: BString },
    Input { recipe: EvalRecipeRef },
    OutputPath,
    ResourceDir,
    InputResourceDirs,
    HomeDir,
    WorkDir,
    TempDir,
    CaCertificateBundlePath,
}

impl EvalProcessTemplateComponent {
    fn push_recipe_refs(&self, recipe_refs: &mut Vec<EvalRecipeRef>) {
        match self {
            Self::Input { recipe } => {
                recipe_refs.push(*recipe);
            }
            Self::Literal { value: _ }
            | Self::OutputPath
            | Self::ResourceDir
            | Self::InputResourceDirs
            | Self::HomeDir
            | Self::WorkDir
            | Self::TempDir
            | Self::CaCertificateBundlePath => {}
        }
    }
}

pub(super) async fn deserialize_recipe(
    brioche: &crate::Brioche,
    js_runtime: &mut deno_core::JsRuntime,
    state: &mut EvalRecipeState,
    recipe: JsValue,
) -> Result<RecipeRef, DeserializeError> {
    let scope = recipe.scope.clone();
    let recipe_ref = {
        deno_core::scope!(js_scope, js_runtime);
        let brioche = &mut *brioche.write().await;

        let recipe_ref = deserialize_partial_recipe(brioche, js_scope, state, recipe)?;

        if let Some(recipe) = finish_recipe(brioche, state, recipe_ref)? {
            return Ok(recipe);
        }

        recipe_ref
    };

    loop {
        js_runtime
            .run_event_loop(deno_core::PollEventLoopOptions::default())
            .await
            .map_err(|error| DeserializeError::new(error, scope.clone()))?;

        deno_core::scope!(js_scope, js_runtime);
        let brioche = &mut *brioche.write().await;

        let did_resolve_any = eval_recipes_tick(brioche, js_scope, state)?;

        if let Some(recipe) = finish_recipe(brioche, state, recipe_ref)? {
            return Ok(recipe);
        }

        if !did_resolve_any {
            return Err(DeserializeError::new(
                JsRuntimeError::UnknownEvalError {
                    reason: "stuck waiting for promises to resolve".into(),
                },
                scope,
            ));
        }
    }
}

fn deserialize_partial_recipe(
    brioche: &mut crate::BriocheState,
    js_scope: &mut deno_core::v8::PinScope,
    state: &mut EvalRecipeState,
    recipe: JsValue,
) -> Result<EvalRecipeRef, DeserializeError> {
    deserialize_partial_recipe_to(brioche, js_scope, state, recipe, None)
}

fn deserialize_partial_recipe_to(
    brioche: &mut crate::BriocheState,
    js_scope: &mut deno_core::v8::PinScope,
    state: &mut EvalRecipeState,
    recipe: JsValue,
    pending_recipe_ref: Option<EvalRecipeRef>,
) -> Result<EvalRecipeRef, DeserializeError> {
    if let Some(recipe_ref) = state.recipes_by_value.get(&recipe.value) {
        return Ok(*recipe_ref);
    }

    let value = deno_core::v8::Local::new(js_scope, recipe.value);
    let recipe_scope = recipe.scope;

    let (value, scope) = if !recipe_scope.is_in_brioche_serialize_call()
        && let Ok(function) = deno_core::v8::Local::<deno_core::v8::Function>::try_from(value)
    {
        let scope = recipe_scope.clone().with_call();

        let output = state
            .function_outputs
            .get(&deno_core::v8::Global::new(js_scope, function));
        let output = if let Some(output) = output {
            if let Some(recipe_ref) = state.recipes_by_value.get(output).copied() {
                state
                    .recipes_by_value
                    .entry(deno_core::v8::Global::new(js_scope, value))
                    .or_insert(recipe_ref);
                return Ok(recipe_ref);
            }

            deno_core::v8::Local::new(js_scope, output)
        } else {
            deno_core::v8::tc_scope!(let js_scope, js_scope);

            let output = function.call(js_scope, deno_core::v8::undefined(js_scope).into(), &[]);
            let Some(output) = output else {
                if let Some(exception) = js_scope.exception() {
                    return Err(DeserializeError::new(
                        deno_core::error::JsError::from_v8_exception(js_scope, exception),
                        scope,
                    ));
                }
                return Err(DeserializeError::new(
                    JsRuntimeError::UnknownEvalError {
                        reason: "function call failed without an exception".into(),
                    },
                    scope,
                ));
            };

            state.function_outputs.insert(
                deno_core::v8::Global::new(js_scope, function),
                deno_core::v8::Global::new(js_scope, output),
            );
            output
        };

        (output, scope)
    } else {
        (value, recipe_scope.clone())
    };

    let promise = deno_core::v8::Local::<deno_core::v8::Promise>::try_from(value);
    let value = if let Ok(promise) = promise {
        match promise.state() {
            deno_core::v8::PromiseState::Pending => {
                let node_weight = EvalRecipeNode::PendingRecipe {
                    value: deno_core::v8::Global::new(js_scope, promise),
                    scope,
                };
                let recipe_ref = if let Some(pending_recipe_ref) = pending_recipe_ref {
                    let pending_node_weight =
                        state.graph.node_weight_mut(pending_recipe_ref.0).unwrap();
                    *pending_node_weight = node_weight;
                    pending_recipe_ref
                } else {
                    EvalRecipeRef(state.graph.add_node(node_weight))
                };

                state
                    .recipes_by_value
                    .insert(deno_core::v8::Global::new(js_scope, value), recipe_ref);
                return Ok(recipe_ref);
            }
            deno_core::v8::PromiseState::Rejected => {
                let rejection = promise.result(js_scope);
                let message = get_rejection_message(js_scope, rejection);
                return Err(DeserializeError::new(
                    JsRuntimeError::PromiseRejected { message },
                    scope,
                ));
            }
            deno_core::v8::PromiseState::Fulfilled => {
                let resolved = promise.result(js_scope);

                let recipe_ref = state
                    .recipes_by_value
                    .get(&deno_core::v8::Global::new(js_scope, resolved))
                    .copied();
                if let Some(recipe_ref) = recipe_ref {
                    state
                        .recipes_by_value
                        .entry(deno_core::v8::Global::new(js_scope, value))
                        .or_insert(recipe_ref);
                    return Ok(recipe_ref);
                }

                resolved
            }
        }
    } else {
        value
    };

    let (value, scope) = if !scope.is_in_brioche_serialize_call()
        && let Ok(object) = deno_core::v8::Local::<deno_core::v8::Object>::try_from(value)
        && let Some(brioche_serialize_value) = object.get(
            js_scope,
            js_string(js_scope, "briocheSerialize", &scope)?.into(),
        )
        && !brioche_serialize_value.is_null_or_undefined()
    {
        let brioche_serialize =
            deno_core::v8::Local::<deno_core::v8::Function>::try_from(brioche_serialize_value)
                .map_err(|_| {
                    DeserializeError::new(
                        JsRuntimeError::TypeError {
                            expected: "function".into(),
                            actual: brioche_serialize_value.type_repr().into(),
                        },
                        scope.clone().with_field("briocheSerialize"),
                    )
                })?;

        let scope = scope.with_brioche_serialize_call();

        let brioche_serialize_value = deno_core::v8::Global::new(js_scope, brioche_serialize_value);
        if let Some(recipe_ref) = state
            .recipes_by_value
            .get(&brioche_serialize_value)
            .copied()
        {
            state
                .recipes_by_value
                .insert(brioche_serialize_value, recipe_ref);
            return Ok(recipe_ref);
        }

        deno_core::v8::tc_scope!(let js_scope, js_scope);

        let output = brioche_serialize.call(js_scope, value, &[]);
        let Some(output) = output else {
            if let Some(exception) = js_scope.exception() {
                return Err(DeserializeError::new(
                    deno_core::error::JsError::from_v8_exception(js_scope, exception),
                    scope,
                ));
            }
            return Err(DeserializeError::new(
                JsRuntimeError::UnknownEvalError {
                    reason: "function call failed without an exception".into(),
                },
                scope,
            ));
        };

        let promise = deno_core::v8::Local::<deno_core::v8::Promise>::try_from(output);
        let output = if let Ok(promise) = promise {
            match promise.state() {
                deno_core::v8::PromiseState::Pending => {
                    let node_weight = EvalRecipeNode::PendingRecipe {
                        value: deno_core::v8::Global::new(js_scope, promise),
                        scope,
                    };
                    let recipe_ref = if let Some(pending_recipe_ref) = pending_recipe_ref {
                        let pending_node_weight =
                            state.graph.node_weight_mut(pending_recipe_ref.0).unwrap();
                        *pending_node_weight = node_weight;
                        pending_recipe_ref
                    } else {
                        EvalRecipeRef(state.graph.add_node(node_weight))
                    };

                    state
                        .recipes_by_value
                        .insert(deno_core::v8::Global::new(js_scope, value), recipe_ref);
                    return Ok(recipe_ref);
                }
                deno_core::v8::PromiseState::Rejected => {
                    let rejection = promise.result(js_scope);
                    let message = get_rejection_message(js_scope, rejection);
                    return Err(DeserializeError::new(
                        JsRuntimeError::PromiseRejected { message },
                        scope,
                    ));
                }
                deno_core::v8::PromiseState::Fulfilled => {
                    let resolved = promise.result(js_scope);

                    let recipe_ref = state
                        .recipes_by_value
                        .get(&deno_core::v8::Global::new(js_scope, resolved))
                        .copied();
                    if let Some(recipe_ref) = recipe_ref {
                        state
                            .recipes_by_value
                            .entry(deno_core::v8::Global::new(js_scope, value))
                            .or_insert(recipe_ref);
                        return Ok(recipe_ref);
                    }

                    resolved
                }
            }
        } else {
            output
        };

        (output, scope)
    } else if recipe_scope.is_top_level() {
        return Err(DeserializeError::new(JsRuntimeError::MissingField, scope));
    } else {
        (value, scope)
    };

    let value = deno_core::v8::Global::new(js_scope, value);
    let partial_recipe = deserialize_recipe_value(
        brioche,
        js_scope,
        state,
        JsValue {
            value: value.clone(),
            scope: scope.clone(),
        },
    )?;

    let mut edge_refs = vec![];
    partial_recipe.push_recipe_refs(&mut edge_refs);

    let node_weight = EvalRecipeNode::Recipe {
        recipe: Arc::new(partial_recipe),
        scope,
    };

    let partial_recipe_ref = if let Some(pending_recipe_ref) = pending_recipe_ref {
        let pending_node_weight = state.graph.node_weight_mut(pending_recipe_ref.0).unwrap();
        *pending_node_weight = node_weight;
        pending_recipe_ref
    } else {
        EvalRecipeRef(state.graph.add_node(node_weight))
    };

    for edge_ref in edge_refs {
        state
            .graph
            .update_edge(partial_recipe_ref.0, edge_ref.0, EvalRecipeEdge);
    }

    state.recipes_by_value.insert(value, partial_recipe_ref);

    Ok(partial_recipe_ref)
}

fn deserialize_recipe_value(
    brioche: &mut crate::BriocheState,
    js_scope: &mut deno_core::v8::PinScope,
    state: &mut EvalRecipeState,
    value: JsValue,
) -> Result<EvalRecipe, DeserializeError> {
    let (value, kind) = value.get_tag::<RecipeKind>(js_scope, "type")?;
    let recipe = match kind {
        RecipeKind::File => todo!(),
        RecipeKind::Directory => {
            let entry_values = value
                .get_field(js_scope, "entries")?
                .into_object(js_scope)?;

            if let Some((entry_name, _)) = entry_values.iter().next() {
                let scope = value.scope.clone().with_field(entry_name.clone());
                return Err(DeserializeError::new(
                    JsRuntimeError::InvalidValue {
                        reason: "unsupported directory entry value".into(),
                    },
                    scope,
                ));
            }

            let entries = BTreeMap::new();
            EvalRecipe::Directory { entries }
        }
        RecipeKind::Symlink => {
            let target = value
                .get_field(js_scope, "target")?
                .deserialize_tick_encoding(js_scope)?;
            EvalRecipe::Symlink(crate::recipe::Symlink { target })
        }
        RecipeKind::Download => {
            let url = value.get_field(js_scope, "url")?;
            let url = url
                .to_string(js_scope)?
                .parse()
                .map_err(|error| DeserializeError::new(error, url.scope.clone()))?;

            let hash = value.get_field(js_scope, "hash")?;
            let hash = deserialize_hash(js_scope, hash)?;

            EvalRecipe::Download(crate::recipe::DownloadRecipe { url, hash })
        }
        RecipeKind::Unarchive => {
            let file = value.get_field(js_scope, "file")?;
            let file = deserialize_partial_recipe(brioche, js_scope, state, file)?;

            let archive = value
                .get_field(js_scope, "archive")?
                .deserialize_value(js_scope)?;

            let compression = value
                .get_field_or_nullish(js_scope, "compression")?
                .map(|compression| compression.deserialize_value(js_scope))
                .transpose()?
                .unwrap_or(crate::recipe::CompressionFormat::None);

            EvalRecipe::Unarchive {
                file,
                archive,
                compression,
            }
        }
        RecipeKind::Process => {
            let command = value.get_field(js_scope, "command")?;

            let args_values = value.get_field(js_scope, "args")?.into_array(js_scope)?;
            let mut args = vec![];
            for arg in args_values {
                let arg = deserialize_process_template(brioche, js_scope, state, &arg)?;
                args.push(arg);
            }

            let mut env = BTreeMap::new();
            let env_values = value.get_field(js_scope, "env")?.into_object(js_scope)?;
            for (env_var, env_value) in env_values {
                let env_var = tick_encoding::decode(env_var.as_bytes()).map_err(|error| {
                    DeserializeError::new(error, value.scope.clone().with_field("env"))
                })?;
                let env_var = bstr::BString::new(env_var.into_owned());

                let env_value = deserialize_process_template(brioche, js_scope, state, &env_value)?;

                env.insert(env_var, env_value);
            }

            let current_dir = value.get_field_or_nullish(js_scope, "currentDir")?;
            let current_dir = if let Some(current_dir) = current_dir {
                deserialize_process_template(brioche, js_scope, state, &current_dir)?
            } else {
                EvalProcessTemplate::default_current_dir()
            };

            let dependencies = value.get_field_or_nullish(js_scope, "dependencies")?;
            let dependencies = if let Some(dependency_values) = dependencies {
                let dependency_values = dependency_values.into_array(js_scope)?;
                let mut dependencies = vec![];

                for item in dependency_values {
                    let dependency = deserialize_partial_recipe(brioche, js_scope, state, item)?;
                    dependencies.push(dependency);
                }

                dependencies
            } else {
                vec![]
            };

            let work_dir = value.get_field(js_scope, "workDir")?;
            let work_dir = deserialize_partial_recipe(brioche, js_scope, state, work_dir)?;

            let output_scaffold = value.get_field_or_nullish(js_scope, "outputScaffold")?;
            let output_scaffold = if let Some(output_scaffold) = output_scaffold {
                Some(deserialize_partial_recipe(
                    brioche,
                    js_scope,
                    state,
                    output_scaffold,
                )?)
            } else {
                None
            };

            EvalRecipe::Process {
                command: deserialize_process_template(brioche, js_scope, state, &command)?,
                args,
                env,
                current_dir,
                dependencies,
                work_dir,
                output_scaffold,
                platform: value
                    .get_field(js_scope, "platform")?
                    .deserialize_value(js_scope)?,
                is_unsafe: value
                    .get_field_or_nullish(js_scope, "isUnsafe")?
                    .map(|value| value.deserialize_value(js_scope))
                    .transpose()?
                    .unwrap_or(false),
                networking: value
                    .get_field_or_nullish(js_scope, "networking")?
                    .map(|value| value.deserialize_value(js_scope))
                    .transpose()?
                    .unwrap_or(false),
            }
        }
        RecipeKind::CompleteProcess => todo!(),
        RecipeKind::CreateFile => {
            let resources = value.get_field_or_nullish(js_scope, "resources")?;
            let resources = if let Some(resources) = resources {
                Some(deserialize_partial_recipe(
                    brioche, js_scope, state, resources,
                )?)
            } else {
                None
            };

            EvalRecipe::CreateFile {
                content: value
                    .get_field(js_scope, "content")?
                    .deserialize_tick_encoding(js_scope)?,
                executable: value
                    .get_field(js_scope, "executable")?
                    .deserialize_value(js_scope)?,
                resources,
            }
        }
        RecipeKind::CreateDirectory => {
            let entry_values = value
                .get_field(js_scope, "entries")?
                .into_object(js_scope)?;

            let mut entries = BTreeMap::new();
            for (entry_name, entry_value) in entry_values {
                let entry_name = tick_encoding::decode(entry_name.as_bytes()).map_err(|error| {
                    DeserializeError::new(error, value.scope.clone().with_field("entries"))
                })?;
                let entry_name = bstr::BString::new(entry_name.into_owned());

                let entry_value =
                    deserialize_partial_recipe(brioche, js_scope, state, entry_value)?;

                entries.insert(entry_name, entry_value);
            }

            EvalRecipe::CreateDirectory { entries }
        }
        RecipeKind::Cast => {
            let recipe = value.get_field(js_scope, "recipe")?;
            let recipe = deserialize_partial_recipe(brioche, js_scope, state, recipe)?;

            EvalRecipe::Cast {
                recipe,
                to: value
                    .get_field(js_scope, "to")?
                    .deserialize_value(js_scope)?,
            }
        }
        RecipeKind::Merge => {
            let directory_values = value
                .get_field(js_scope, "directories")?
                .into_array(js_scope)?;
            let mut directories = vec![];

            for directory in directory_values {
                let directory = deserialize_partial_recipe(brioche, js_scope, state, directory)?;
                directories.push(directory);
            }

            EvalRecipe::Merge { directories }
        }
        RecipeKind::Peel => {
            let directory = value.get_field(js_scope, "directory")?;
            let directory = deserialize_partial_recipe(brioche, js_scope, state, directory)?;

            let depth = value
                .get_field(js_scope, "depth")?
                .deserialize_value(js_scope)?;

            EvalRecipe::Peel { directory, depth }
        }
        RecipeKind::Get => {
            let directory = value.get_field(js_scope, "directory")?;
            let directory = deserialize_partial_recipe(brioche, js_scope, state, directory)?;

            let path = value
                .get_field(js_scope, "path")?
                .deserialize_tick_encoding(js_scope)?;

            EvalRecipe::Get { directory, path }
        }
        RecipeKind::Insert => {
            let directory = value.get_field(js_scope, "directory")?;
            let directory = deserialize_partial_recipe(brioche, js_scope, state, directory)?;

            let path = value
                .get_field(js_scope, "path")?
                .deserialize_tick_encoding(js_scope)?;

            let recipe = value.get_field_or_nullish(js_scope, "recipe")?;
            let recipe = if let Some(recipe) = recipe {
                Some(deserialize_partial_recipe(
                    brioche, js_scope, state, recipe,
                )?)
            } else {
                None
            };

            EvalRecipe::Insert {
                directory,
                path,
                recipe,
            }
        }
        RecipeKind::Glob => todo!(),
        RecipeKind::SetPermissions => {
            let file = value.get_field(js_scope, "file")?;
            let file = deserialize_partial_recipe(brioche, js_scope, state, file)?;

            let executable = value
                .get_field_or_nullish(js_scope, "executable")?
                .map(|executable| executable.deserialize_value(js_scope))
                .transpose()?;

            EvalRecipe::SetPermissions { file, executable }
        }
        RecipeKind::CollectReferences => todo!(),
        RecipeKind::AttachResources => todo!(),
        RecipeKind::Proxy => todo!(),
        RecipeKind::Sync => todo!(),
    };

    Ok(recipe)
}

fn deserialize_process_template(
    brioche: &mut crate::BriocheState,
    js_scope: &mut deno_core::v8::PinScope,
    state: &mut EvalRecipeState,
    value: &JsValue,
) -> Result<EvalProcessTemplate, DeserializeError> {
    let component_values = value
        .get_field(js_scope, "components")?
        .into_array(js_scope)?;
    let mut components = vec![];

    for component in component_values {
        let component =
            deserialize_process_template_component(brioche, js_scope, state, component)?;
        components.push(component);
    }

    Ok(EvalProcessTemplate { components })
}

fn deserialize_process_template_component(
    brioche: &mut crate::BriocheState,
    js_scope: &mut deno_core::v8::PinScope,
    state: &mut EvalRecipeState,
    value: JsValue,
) -> Result<EvalProcessTemplateComponent, DeserializeError> {
    let (value, kind) = value.get_tag::<ProcessTemplateComponentKind>(js_scope, "type")?;
    match kind {
        ProcessTemplateComponentKind::Literal => {
            let value = value.get_field(js_scope, "value")?;
            let value = value.deserialize_tick_encoding(js_scope)?;
            Ok(EvalProcessTemplateComponent::Literal { value })
        }
        ProcessTemplateComponentKind::Input => {
            let recipe = value.get_field(js_scope, "recipe")?;
            let recipe = deserialize_partial_recipe(brioche, js_scope, state, recipe)?;
            Ok(EvalProcessTemplateComponent::Input { recipe })
        }
        ProcessTemplateComponentKind::OutputPath => Ok(EvalProcessTemplateComponent::OutputPath),
        ProcessTemplateComponentKind::ResourceDir => Ok(EvalProcessTemplateComponent::ResourceDir),
        ProcessTemplateComponentKind::InputResourceDirs => {
            Ok(EvalProcessTemplateComponent::InputResourceDirs)
        }
        ProcessTemplateComponentKind::HomeDir => Ok(EvalProcessTemplateComponent::HomeDir),
        ProcessTemplateComponentKind::WorkDir => Ok(EvalProcessTemplateComponent::WorkDir),
        ProcessTemplateComponentKind::TempDir => Ok(EvalProcessTemplateComponent::TempDir),
        ProcessTemplateComponentKind::CaCertificateBundlePath => {
            Ok(EvalProcessTemplateComponent::CaCertificateBundlePath)
        }
    }
}

fn deserialize_hash(
    js_scope: &mut deno_core::v8::PinScope,
    value: JsValue,
) -> Result<crate::hash::AnyHash, DeserializeError> {
    let (value, kind) = value.get_tag::<AnyHashKind>(js_scope, "type")?;
    match kind {
        AnyHashKind::Sha256 => {
            let value = value.get_field(js_scope, "value")?;
            let value = value
                .to_string(js_scope)?
                .parse()
                .map_err(|error| DeserializeError::new(error, value.scope.clone()))?;
            Ok(crate::hash::AnyHash::Sha256 { value })
        }
    }
}

fn finish_recipe(
    brioche: &mut crate::BriocheState,
    state: &mut EvalRecipeState,
    recipe_ref: EvalRecipeRef,
) -> Result<Option<crate::recipe::RecipeRef>, DeserializeError> {
    if let Some(recipe_ref) = state.recipe_to_finished_recipe.get(&recipe_ref) {
        return Ok(Some(*recipe_ref));
    }

    let mut graph = state.graph.clone();
    let mut dfs_space = petgraph::algo::DfsSpace::default();
    graph.retain_nodes(|graph, index| {
        petgraph::algo::has_path_connecting(&*graph, recipe_ref.0, index, Some(&mut dfs_space))
    });

    let all_complete = graph
        .node_weights()
        .all(|node| matches!(node, EvalRecipeNode::Recipe { .. }));
    if !all_complete {
        return Ok(None);
    }

    let scope = graph[recipe_ref.0].scope();
    let node_indices = petgraph::algo::toposort(&graph, None).map_err(|_| {
        DeserializeError::new(
            JsRuntimeError::InvalidValue {
                reason: "recipe has a circular dependency".into(),
            },
            scope.clone(),
        )
    })?;

    for index in node_indices.iter().copied().rev() {
        let recipe_ref = EvalRecipeRef(index);
        let recipe = match &graph[index] {
            EvalRecipeNode::Recipe { recipe, .. } => recipe,
            EvalRecipeNode::PendingRecipe { .. } => {
                unreachable!("encountered incomplete recipe node");
            }
        };

        let finished_recipe = build_recipe(recipe, &state.recipe_to_finished_recipe);
        let finished_recipe_ref = brioche
            .recipes_mut()
            .insert_recipe(Arc::new(finished_recipe));
        state
            .recipe_to_finished_recipe
            .insert(recipe_ref, finished_recipe_ref);
    }

    Ok(Some(state.recipe_to_finished_recipe[&recipe_ref]))
}

fn build_recipe(
    recipe: &EvalRecipe,
    recipe_to_finished_recipe: &HashMap<EvalRecipeRef, RecipeRef>,
) -> crate::recipe::Recipe {
    match recipe {
        EvalRecipe::File {
            content_blob,
            executable,
            resources,
        } => Recipe::File(crate::recipe::File {
            content_blob: *content_blob,
            executable: *executable,
            resources: resources.map(|resources| recipe_to_finished_recipe[&resources]),
        }),
        EvalRecipe::Directory { entries } => Recipe::Directory(crate::recipe::Directory {
            entries: entries
                .iter()
                .map(|(key, value)| (key.clone(), recipe_to_finished_recipe[value]))
                .collect(),
        }),
        EvalRecipe::Symlink(symlink) => Recipe::Symlink(symlink.clone()),
        EvalRecipe::Download(download) => Recipe::Download(download.clone()),
        EvalRecipe::Unarchive {
            file,
            archive,
            compression,
        } => Recipe::Unarchive(crate::recipe::UnarchiveRecipe {
            file: recipe_to_finished_recipe[file],
            archive: *archive,
            compression: *compression,
        }),
        EvalRecipe::Process {
            command,
            args,
            env,
            current_dir,
            dependencies,
            work_dir,
            output_scaffold,
            platform,
            is_unsafe,
            networking,
        } => Recipe::Process(crate::recipe::ProcessRecipe {
            command: build_process_template(command, recipe_to_finished_recipe),
            args: args
                .iter()
                .map(|arg| build_process_template(arg, recipe_to_finished_recipe))
                .collect(),
            env: env
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        build_process_template(value, recipe_to_finished_recipe),
                    )
                })
                .collect(),
            current_dir: build_process_template(current_dir, recipe_to_finished_recipe),
            dependencies: dependencies
                .iter()
                .map(|recipe_ref| recipe_to_finished_recipe[recipe_ref])
                .collect(),
            work_dir: recipe_to_finished_recipe[work_dir],
            output_scaffold: output_scaffold
                .map(|recipe_ref| recipe_to_finished_recipe[&recipe_ref]),
            platform: *platform,
            is_unsafe: *is_unsafe,
            networking: *networking,
        }),
        EvalRecipe::CreateFile {
            content,
            executable,
            resources,
        } => Recipe::CreateFile {
            content: content.clone(),
            executable: *executable,
            resources: resources.map(|resources| recipe_to_finished_recipe[&resources]),
        },
        EvalRecipe::CreateDirectory { entries } => Recipe::CreateDirectory {
            entries: entries
                .iter()
                .map(|(key, value)| (key.clone(), recipe_to_finished_recipe[value]))
                .collect(),
        },
        EvalRecipe::Cast { recipe, to } => Recipe::Cast {
            recipe: recipe_to_finished_recipe[recipe],
            to: *to,
        },
        EvalRecipe::Merge { directories } => Recipe::Merge {
            directories: directories
                .iter()
                .map(|recipe_ref| recipe_to_finished_recipe[recipe_ref])
                .collect(),
        },
        EvalRecipe::Peel { directory, depth } => Recipe::Peel {
            directory: recipe_to_finished_recipe[directory],
            depth: *depth,
        },
        EvalRecipe::Get { directory, path } => Recipe::Get {
            directory: recipe_to_finished_recipe[directory],
            path: path.clone(),
        },
        EvalRecipe::Insert {
            directory,
            path,
            recipe,
        } => Recipe::Insert {
            directory: recipe_to_finished_recipe[directory],
            path: path.clone(),
            recipe: recipe.map(|recipe_ref| recipe_to_finished_recipe[&recipe_ref]),
        },
        EvalRecipe::Glob {
            directory,
            patterns,
        } => Recipe::Glob {
            directory: recipe_to_finished_recipe[directory],
            patterns: patterns.clone(),
        },
        EvalRecipe::SetPermissions { file, executable } => Recipe::SetPermissions {
            file: recipe_to_finished_recipe[file],
            executable: *executable,
        },
        EvalRecipe::CollectReferences { recipe } => Recipe::CollectReferences {
            recipe: recipe_to_finished_recipe[recipe],
        },
        EvalRecipe::AttachResources { recipe } => Recipe::AttachResources {
            recipe: recipe_to_finished_recipe[recipe],
        },
        EvalRecipe::Proxy { recipe } => Recipe::Proxy {
            recipe: recipe_to_finished_recipe[recipe],
        },
        EvalRecipe::Sync { recipe } => Recipe::Sync {
            recipe: recipe_to_finished_recipe[recipe],
        },
    }
}

fn build_process_template(
    process_template: &EvalProcessTemplate,
    recipe_to_finished_recipe: &HashMap<EvalRecipeRef, RecipeRef>,
) -> crate::recipe::ProcessTemplate {
    let components = process_template
        .components
        .iter()
        .map(|component| match component {
            EvalProcessTemplateComponent::Literal { value } => {
                crate::recipe::ProcessTemplateComponent::Literal {
                    value: value.clone(),
                }
            }
            EvalProcessTemplateComponent::Input { recipe } => {
                crate::recipe::ProcessTemplateComponent::Input {
                    recipe: recipe_to_finished_recipe[recipe],
                }
            }
            EvalProcessTemplateComponent::OutputPath => {
                crate::recipe::ProcessTemplateComponent::OutputPath
            }
            EvalProcessTemplateComponent::ResourceDir => {
                crate::recipe::ProcessTemplateComponent::ResourceDir
            }
            EvalProcessTemplateComponent::InputResourceDirs => {
                crate::recipe::ProcessTemplateComponent::InputResourceDirs
            }
            EvalProcessTemplateComponent::HomeDir => {
                crate::recipe::ProcessTemplateComponent::HomeDir
            }
            EvalProcessTemplateComponent::WorkDir => {
                crate::recipe::ProcessTemplateComponent::WorkDir
            }
            EvalProcessTemplateComponent::TempDir => {
                crate::recipe::ProcessTemplateComponent::TempDir
            }
            EvalProcessTemplateComponent::CaCertificateBundlePath => {
                crate::recipe::ProcessTemplateComponent::CaCertificateBundlePath
            }
        })
        .collect();

    crate::recipe::ProcessTemplate { components }
}

#[derive(Debug, Clone)]
pub(super) struct JsValue {
    value: deno_core::v8::Global<deno_core::v8::Value>,
    scope: JsValueScope,
}

impl JsValue {
    pub(super) const fn new(
        value: deno_core::v8::Global<deno_core::v8::Value>,
        scope: JsValueScope,
    ) -> Self {
        Self { value, scope }
    }

    fn deserialize_value<T>(
        self,
        js_scope: &mut deno_core::v8::PinScope,
    ) -> Result<T, DeserializeError>
    where
        T: DeserializeV8,
    {
        let value = deno_core::v8::Local::new(js_scope, self.value);
        T::deserialize(js_scope, value, &self.scope)
    }

    fn deserialize_tick_encoding(
        self,
        js_scope: &mut deno_core::v8::PinScope,
    ) -> Result<bstr::BString, DeserializeError> {
        let value = deno_core::v8::Local::new(js_scope, self.value);
        let value =
            deno_core::v8::Local::<deno_core::v8::String>::try_from(value).map_err(|_| {
                DeserializeError::type_error("string", value.type_repr(), self.scope.clone())
            })?;
        let string = value.to_rust_string_lossy(js_scope);
        let mut bytes = string.into_bytes();
        let decoded = tick_encoding::decode_in_place(&mut bytes)
            .map_err(|error| DeserializeError::new(error, self.scope.clone()))?;
        let decoded_len = decoded.len();
        bytes.truncate(decoded_len);
        Ok(bstr::BString::new(bytes))
    }

    fn try_get_field(
        &self,
        js_scope: &mut deno_core::v8::PinScope,
        key: impl Into<Cow<'static, str>>,
    ) -> Result<Option<Self>, DeserializeError> {
        let key = key.into();
        let key_string = deno_core::v8::String::new(js_scope, &key).ok_or_else(|| {
            DeserializeError::new(
                JsRuntimeError::InvalidJsString(key.clone()),
                self.scope.clone(),
            )
        })?;

        let value = deno_core::v8::Local::new(js_scope, self.value.clone());
        let value =
            deno_core::v8::Local::<deno_core::v8::Object>::try_from(value).map_err(|_| {
                DeserializeError::type_error("object", value.type_repr(), self.scope.clone())
            })?;

        let has_key = value.has(js_scope, key_string.into());
        let has_key = matches!(has_key, Some(true));
        if !has_key {
            return Ok(None);
        }
        let field = value.get(js_scope, key_string.into());
        let Some(field) = field else {
            return Ok(None);
        };

        Ok(Some(Self {
            value: deno_core::v8::Global::new(js_scope, field),
            scope: self.scope.clone().with_field(key),
        }))
    }

    fn get_field_or_nullish(
        &self,
        js_scope: &mut deno_core::v8::PinScope,
        key: impl Into<Cow<'static, str>>,
    ) -> Result<Option<Self>, DeserializeError> {
        let key = key.into();
        let key_string = deno_core::v8::String::new(js_scope, &key).ok_or_else(|| {
            DeserializeError::new(
                JsRuntimeError::InvalidJsString(key.clone()),
                self.scope.clone(),
            )
        })?;

        let value = deno_core::v8::Local::new(js_scope, self.value.clone());
        let value =
            deno_core::v8::Local::<deno_core::v8::Object>::try_from(value).map_err(|_| {
                DeserializeError::type_error("object", value.type_repr(), self.scope.clone())
            })?;

        let field = value.get(js_scope, key_string.into());
        let Some(field) = field else {
            return Ok(None);
        };
        if field.is_null_or_undefined() {
            return Ok(None);
        }

        Ok(Some(Self {
            value: deno_core::v8::Global::new(js_scope, field),
            scope: self.scope.clone().with_field(key),
        }))
    }

    fn get_field(
        &self,
        js_scope: &mut deno_core::v8::PinScope,
        key: impl Into<Cow<'static, str>>,
    ) -> Result<Self, DeserializeError> {
        let key = key.into();
        let scope = self.scope.clone();
        let field = self.try_get_field(js_scope, key.clone())?.ok_or_else(|| {
            DeserializeError::new(JsRuntimeError::MissingField, scope.with_field(key))
        })?;
        Ok(field)
    }

    fn get_tag<T>(
        self,
        js_scope: &mut deno_core::v8::PinScope,
        tag_key: impl Into<Cow<'static, str>>,
    ) -> Result<(Self, T), DeserializeError>
    where
        T: JsEnumTag,
    {
        let tag_key = tag_key.into();
        let tag_key_string = deno_core::v8::String::new(js_scope, &tag_key).ok_or_else(|| {
            DeserializeError::new(
                JsRuntimeError::InvalidJsString(tag_key.clone()),
                self.scope.clone(),
            )
        })?;

        let value = deno_core::v8::Local::new(js_scope, self.value.clone());
        let value =
            deno_core::v8::Local::<deno_core::v8::Object>::try_from(value).map_err(|_| {
                DeserializeError::type_error("object", value.type_repr(), self.scope.clone())
            })?;

        let tag_value = value.get(js_scope, tag_key_string.into());
        let Some(tag_value) = tag_value else {
            return Err(DeserializeError::new(
                JsRuntimeError::MissingField,
                self.scope.clone().with_field(tag_key),
            ));
        };
        let tag = T::deserialize(js_scope, tag_value, &self.scope.clone().with_field(tag_key))?;

        Ok((
            Self {
                value: self.value,
                scope: self.scope.with_variant(T::tag(&tag)),
            },
            tag,
        ))
    }

    fn into_array(
        self,
        js_scope: &mut deno_core::v8::PinScope,
    ) -> Result<Vec<Self>, DeserializeError> {
        let value = deno_core::v8::Local::new(js_scope, &self.value);
        let value =
            deno_core::v8::Local::<deno_core::v8::Array>::try_from(value).map_err(|_| {
                DeserializeError::type_error("array", value.type_repr(), self.scope.clone())
            })?;

        let mut items = vec![];
        for i in 0..value.length() {
            let scope = self.scope.clone().with_index(i);
            let item = value.get_index(js_scope, i).ok_or_else(|| {
                DeserializeError::new(
                    JsRuntimeError::InvalidValue {
                        reason: "index not set".into(),
                    },
                    scope.clone(),
                )
            })?;
            let item = deno_core::v8::Global::new(js_scope, item);
            items.push(Self { value: item, scope });
        }

        Ok(items)
    }

    fn into_object(
        self,
        js_scope: &mut deno_core::v8::PinScope,
    ) -> Result<HashMap<String, Self>, DeserializeError> {
        let value = deno_core::v8::Local::new(js_scope, &self.value);

        if value.is_array() {
            return Err(DeserializeError::type_error(
                "object",
                "array",
                self.scope.clone(),
            ));
        }

        let value =
            deno_core::v8::Local::<deno_core::v8::Object>::try_from(value).map_err(|_| {
                DeserializeError::type_error("object", value.type_repr(), self.scope.clone())
            })?;

        let mut entries = HashMap::new();

        let properties = value
            .get_own_property_names(
                js_scope,
                deno_core::v8::GetPropertyNamesArgs {
                    mode: deno_core::v8::KeyCollectionMode::OwnOnly,
                    property_filter: deno_core::v8::PropertyFilter::ALL_PROPERTIES,
                    index_filter: deno_core::v8::IndexFilter::SkipIndices,
                    key_conversion: deno_core::v8::KeyConversionMode::ConvertToString,
                },
            )
            .ok_or_else(|| {
                DeserializeError::new(
                    JsRuntimeError::InvalidValue {
                        reason: "could not get object properties".into(),
                    },
                    self.scope.clone(),
                )
            })?;
        for i in 0..properties.length() {
            let Some(property) = properties.get_index(js_scope, i) else {
                continue;
            };
            let property_string = deno_core::v8::Local::<deno_core::v8::String>::try_from(property)
                .map_err(|_| {
                    DeserializeError::new(
                        JsRuntimeError::InvalidValue {
                            reason: "object has non-string property".into(),
                        },
                        self.scope.clone(),
                    )
                })?;
            let property_string = property_string.to_rust_string_lossy(js_scope);

            let scope = self.scope.clone().with_field(property_string.clone());

            let value = value.get(js_scope, property).ok_or_else(|| {
                DeserializeError::new(
                    JsRuntimeError::InvalidValue {
                        reason: "property does not exist in object".into(),
                    },
                    scope.clone(),
                )
            })?;
            let value = deno_core::v8::Global::new(js_scope, value);
            let value = Self { value, scope };

            entries.insert(property_string, value);
        }

        Ok(entries)
    }

    fn to_string(
        &self,
        js_scope: &mut deno_core::v8::PinScope,
    ) -> Result<String, DeserializeError> {
        let value = deno_core::v8::Local::new(js_scope, self.value.clone());
        let string =
            deno_core::v8::Local::<deno_core::v8::String>::try_from(value).map_err(|_| {
                DeserializeError::type_error("string", value.type_repr(), self.scope.clone())
            })?;
        Ok(string.to_rust_string_lossy(js_scope))
    }
}

fn js_string<'s>(
    js_scope: &'s deno_core::v8::PinScope,
    s: &str,
    scope: &JsValueScope,
) -> Result<deno_core::v8::Local<'s, deno_core::v8::String>, DeserializeError> {
    deno_core::v8::String::new(js_scope, s).ok_or_else(|| {
        DeserializeError::new(
            JsRuntimeError::InvalidJsString(s.to_owned().into()),
            scope.clone(),
        )
    })
}

trait DeserializeV8: Sized {
    fn deserialize(
        js_scope: &mut deno_core::v8::PinScope,
        value: deno_core::v8::Local<deno_core::v8::Value>,
        scope: &JsValueScope,
    ) -> Result<Self, DeserializeError>;
}

impl DeserializeV8 for bool {
    fn deserialize(
        js_scope: &mut deno_core::v8::PinScope,
        value: deno_core::v8::Local<deno_core::v8::Value>,
        scope: &JsValueScope,
    ) -> Result<Self, DeserializeError> {
        let value =
            deno_core::v8::Local::<deno_core::v8::Boolean>::try_from(value).map_err(|_| {
                DeserializeError::type_error("boolean", value.type_repr(), scope.clone())
            })?;
        Ok(value.boolean_value(js_scope))
    }
}

impl DeserializeV8 for f64 {
    fn deserialize(
        _js_scope: &mut deno_core::v8::PinScope,
        value: deno_core::v8::Local<deno_core::v8::Value>,
        scope: &JsValueScope,
    ) -> Result<Self, DeserializeError> {
        let value =
            deno_core::v8::Local::<deno_core::v8::Number>::try_from(value).map_err(|_| {
                DeserializeError::type_error("number", value.type_repr(), scope.clone())
            })?;
        Ok(value.value())
    }
}

impl DeserializeV8 for i64 {
    #[expect(clippy::cast_possible_truncation, clippy::float_cmp)]
    fn deserialize(
        js_scope: &mut deno_core::v8::PinScope,
        value: deno_core::v8::Local<deno_core::v8::Value>,
        scope: &JsValueScope,
    ) -> Result<Self, DeserializeError> {
        let number = f64::deserialize(js_scope, value, scope)?;
        let rounded = number.round();
        if rounded != number {
            return Err(DeserializeError::new(
                JsRuntimeError::InvalidValue {
                    reason: "number is not an integer".into(),
                },
                scope.clone(),
            ));
        }

        Ok(rounded as Self)
    }
}

impl DeserializeV8 for u32 {
    fn deserialize(
        js_scope: &mut deno_core::v8::PinScope,
        value: deno_core::v8::Local<deno_core::v8::Value>,
        scope: &JsValueScope,
    ) -> Result<Self, DeserializeError> {
        let number = i64::deserialize(js_scope, value, scope)?;
        let number =
            Self::try_from(number).map_err(|error| DeserializeError::new(error, scope.clone()))?;
        Ok(number)
    }
}

impl<T> DeserializeV8 for T
where
    T: JsEnumTag,
{
    fn deserialize(
        js_scope: &mut deno_core::v8::PinScope,
        value: deno_core::v8::Local<deno_core::v8::Value>,
        scope: &JsValueScope,
    ) -> Result<Self, DeserializeError> {
        let string =
            deno_core::v8::Local::<deno_core::v8::String>::try_from(value).map_err(|_| {
                DeserializeError::type_error("string", value.type_repr(), scope.clone())
            })?;
        let string = deno_core::v8::ValueView::new(js_scope, string);
        let string = string.to_cow_lossy();
        let value = T::from_str(&string).ok_or_else(|| {
            DeserializeError::new(
                JsRuntimeError::InvalidEnumVariant {
                    expected: T::VALUES.iter().map(T::tag).collect(),
                    got: string.into_owned(),
                },
                scope.clone(),
            )
        })?;
        Ok(value)
    }
}

pub trait JsEnumTag: Sized + 'static {
    const VALUES: &[Self];
    fn tag(&self) -> &'static str;
    fn from_str(value: &str) -> Option<Self>;
}

impl JsEnumTag for RecipeKind {
    const VALUES: &[Self] = &[
        Self::File,
        Self::Directory,
        Self::Symlink,
        Self::Download,
        Self::Unarchive,
        Self::Process,
        Self::CompleteProcess,
        Self::CreateFile,
        Self::CreateDirectory,
        Self::Cast,
        Self::Merge,
        Self::Peel,
        Self::Get,
        Self::Insert,
        Self::Glob,
        Self::SetPermissions,
        Self::CollectReferences,
        Self::AttachResources,
        Self::Proxy,
        Self::Sync,
    ];

    fn tag(&self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::Symlink => "symlink",
            Self::Download => "download",
            Self::Unarchive => "unarchive",
            Self::Process => "process",
            Self::CompleteProcess => "complete_process",
            Self::CreateFile => "create_file",
            Self::CreateDirectory => "create_directory",
            Self::Cast => "cast",
            Self::Merge => "merge",
            Self::Peel => "peel",
            Self::Get => "get",
            Self::Insert => "insert",
            Self::Glob => "glob",
            Self::SetPermissions => "set_permissions",
            Self::CollectReferences => "collect_references",
            Self::AttachResources => "attach_resources",
            Self::Proxy => "proxy",
            Self::Sync => "sync",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "file" => Some(Self::File),
            "directory" => Some(Self::Directory),
            "symlink" => Some(Self::Symlink),
            "download" => Some(Self::Download),
            "unarchive" => Some(Self::Unarchive),
            "process" => Some(Self::Process),
            "complete_process" => Some(Self::CompleteProcess),
            "create_file" => Some(Self::CreateFile),
            "create_directory" => Some(Self::CreateDirectory),
            "cast" => Some(Self::Cast),
            "merge" => Some(Self::Merge),
            "peel" => Some(Self::Peel),
            "get" => Some(Self::Get),
            "insert" => Some(Self::Insert),
            "glob" => Some(Self::Glob),
            "set_permissions" => Some(Self::SetPermissions),
            "collect_references" => Some(Self::CollectReferences),
            "attach_resources" => Some(Self::AttachResources),
            "proxy" => Some(Self::Proxy),
            "sync" => Some(Self::Sync),
            _ => None,
        }
    }
}

impl JsEnumTag for ArtifactKind {
    const VALUES: &[Self] = &[Self::Directory, Self::File, Self::Symlink];

    fn tag(&self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::Symlink => "symlink",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "file" => Some(Self::File),
            "directory" => Some(Self::Directory),
            "symlink" => Some(Self::Symlink),
            _ => None,
        }
    }
}

impl JsEnumTag for crate::platform::Platform {
    const VALUES: &[Self] = &[Self::X86_64Linux, Self::Aarch64Linux];

    fn tag(&self) -> &'static str {
        match self {
            Self::X86_64Linux => "x86_64-linux",
            Self::Aarch64Linux => "aarch64-linux",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "x86_64-linux" => Some(Self::X86_64Linux),
            "aarch64-linux" => Some(Self::Aarch64Linux),
            _ => None,
        }
    }
}

impl JsEnumTag for crate::recipe::ArchiveFormat {
    const VALUES: &[Self] = &[Self::Tar, Self::Zip];

    fn tag(&self) -> &'static str {
        match self {
            Self::Tar => "tar",
            Self::Zip => "zip",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "tar" => Some(Self::Tar),
            "zip" => Some(Self::Zip),
            _ => None,
        }
    }
}

impl JsEnumTag for crate::recipe::CompressionFormat {
    const VALUES: &[Self] = &[Self::None, Self::Bzip2, Self::Gzip, Self::Xz, Self::Zstd];

    fn tag(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Bzip2 => "bzip2",
            Self::Gzip => "gzip",
            Self::Xz => "xz",
            Self::Zstd => "zstd",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "bzip2" => Some(Self::Bzip2),
            "gzip" => Some(Self::Gzip),
            "xz" => Some(Self::Xz),
            "zstd" => Some(Self::Zstd),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ProcessTemplateComponentKind {
    Literal,
    Input,
    OutputPath,
    ResourceDir,
    InputResourceDirs,
    HomeDir,
    WorkDir,
    TempDir,
    CaCertificateBundlePath,
}

impl JsEnumTag for ProcessTemplateComponentKind {
    const VALUES: &[Self] = &[
        Self::Literal,
        Self::Input,
        Self::OutputPath,
        Self::ResourceDir,
        Self::InputResourceDirs,
        Self::HomeDir,
        Self::WorkDir,
        Self::TempDir,
        Self::CaCertificateBundlePath,
    ];

    fn tag(&self) -> &'static str {
        match self {
            Self::Literal => "literal",
            Self::Input => "input",
            Self::OutputPath => "output_path",
            Self::ResourceDir => "resource_dir",
            Self::InputResourceDirs => "input_resource_dirs",
            Self::HomeDir => "home_dir",
            Self::WorkDir => "work_dir",
            Self::TempDir => "temp_dir",
            Self::CaCertificateBundlePath => "ca_certificate_bundle_path",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "literal" => Some(Self::Literal),
            "input" => Some(Self::Input),
            "output_path" => Some(Self::OutputPath),
            "resource_dir" => Some(Self::ResourceDir),
            "input_resource_dirs" => Some(Self::InputResourceDirs),
            "home_dir" => Some(Self::HomeDir),
            "work_dir" => Some(Self::WorkDir),
            "temp_dir" => Some(Self::TempDir),
            "ca_certificate_bundle_path" => Some(Self::CaCertificateBundlePath),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum AnyHashKind {
    Sha256,
}

impl JsEnumTag for AnyHashKind {
    const VALUES: &[Self] = &[Self::Sha256];

    fn tag(&self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "sha256" => Some(Self::Sha256),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct JsValueScope {
    #[expect(unused)]
    module: ModuleRef,
    #[expect(unused)]
    module_path: AbsolutePath,
    export: String,
    components: Vec<ValueScopeComponent>,
}

impl JsValueScope {
    #[must_use]
    pub const fn top_level(module: ModuleRef, module_path: AbsolutePath, export: String) -> Self {
        Self {
            module,
            module_path,
            export,
            components: vec![],
        }
    }

    const fn is_top_level(&self) -> bool {
        self.components.is_empty()
    }

    fn is_in_brioche_serialize_call(&self) -> bool {
        matches!(
            self.components.last(),
            Some(ValueScopeComponent::BriocheSerializeCall)
        )
    }

    fn with_field(mut self, field: impl Into<Cow<'static, str>>) -> Self {
        self.components
            .push(ValueScopeComponent::Field(field.into()));
        self
    }

    fn with_index(mut self, index: u32) -> Self {
        self.components.push(ValueScopeComponent::Index(index));
        self
    }

    fn with_variant(mut self, variant: impl Into<Cow<'static, str>>) -> Self {
        self.components
            .push(ValueScopeComponent::Variant(variant.into()));
        self
    }

    fn with_call(mut self) -> Self {
        self.components.push(ValueScopeComponent::Call);
        self
    }

    fn with_brioche_serialize_call(mut self) -> Self {
        self.components
            .push(ValueScopeComponent::BriocheSerializeCall);
        self
    }

    #[expect(clippy::literal_string_with_formatting_args)]
    fn display_pretty(&self) -> impl std::fmt::Display {
        fn is_safe_field(field: &str) -> bool {
            field.bytes().all(|b| b.is_ascii_alphabetic())
        }

        let components = self
            .components
            .iter()
            .map(|component| {
                lazy_format::lazy_format!(match (component) {
                    ValueScopeComponent::Call => "()",
                    ValueScopeComponent::Variant(variant) => "<{variant}>",
                    ValueScopeComponent::Field(field) if is_safe_field(field) => ".{field}",
                    ValueScopeComponent::Field(field) => "['{field}']",
                    ValueScopeComponent::Index(index) => "[{index}]",
                    ValueScopeComponent::BriocheSerializeCall => ".briocheSerialize()",
                })
            })
            .join_concat();
        lazy_format::lazy_format!("{}{}", self.export, components)
    }
}

#[derive(Debug, Clone)]
enum ValueScopeComponent {
    Call,
    Variant(Cow<'static, str>),
    Field(Cow<'static, str>),
    Index(u32),
    BriocheSerializeCall,
}

fn get_rejection_message(
    js_scope: &mut deno_core::v8::PinScope,
    value: deno_core::v8::Local<deno_core::v8::Value>,
) -> String {
    let value = if let Ok(value) = deno_core::v8::Local::<deno_core::v8::Object>::try_from(value)
        && let Some(message_key) = deno_core::v8::String::new(js_scope, "message")
        && let Some(message) = value.get(js_scope, message_key.into())
    {
        message
    } else {
        value
    };
    value.to_rust_string_lossy(js_scope)
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct DeserializeError(Box<DeserializeErrorInner>);

impl DeserializeError {
    fn new<E>(error: E, scope: JsValueScope) -> Self
    where
        E: Into<JsRuntimeError>,
    {
        Self(Box::new(DeserializeErrorInner {
            scope,
            error: error.into(),
        }))
    }

    fn type_error(expected: &'static str, actual: &'static str, scope: JsValueScope) -> Self {
        Self(Box::new(DeserializeErrorInner {
            scope,
            error: JsRuntimeError::TypeError {
                expected: Cow::Borrowed(expected),
                actual: actual.into(),
            },
        }))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("error deserializing {}: {error}", .scope.display_pretty())]
struct DeserializeErrorInner {
    scope: JsValueScope,
    error: JsRuntimeError,
}
