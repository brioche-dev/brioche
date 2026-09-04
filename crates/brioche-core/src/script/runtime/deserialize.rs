use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use joinery::JoinableIterator as _;

use crate::{
    path::AbsolutePath,
    project::ModuleRef,
    recipe::{ArtifactKind, Recipe, RecipeKind, RecipeRef},
    script::runtime::JsRuntimeError,
};

#[expect(clippy::mutable_key_type)]
pub(super) async fn deserialize_recipe(
    brioche: &crate::Brioche,
    js_runtime: &mut deno_core::JsRuntime,
    value: ValueScope,
    module_namespace: &deno_core::v8::Global<deno_core::v8::Value>,
    cached_recipes: &mut HashMap<deno_core::v8::Global<deno_core::v8::Value>, RecipeRef>,
) -> Result<RecipeRef, DeserializeError> {
    let mut equivalent_values = vec![];

    if let Some(recipe) = cached_recipes.get(&value.value) {
        return Ok(*recipe);
    }
    equivalent_values.push(value.value.clone());

    // If the value is a function, call it. If it returns a promise, resolve it.
    let value = value
        .maybe_call(js_runtime, module_namespace)?
        .maybe_await(js_runtime)
        .await?;

    if let Some(recipe) = cached_recipes.get(&value.value).copied() {
        cached_recipes.extend(equivalent_values.into_iter().map(|value| (value, recipe)));
        return Ok(recipe);
    }
    equivalent_values.push(value.value.clone());

    // If the the value has a `briocheSerialize` method, call and resolve it.
    let brioche_serialize = value
        .clone()
        .get_field_or_nullish(js_runtime, "briocheSerialize")?;
    let value = if let Some(brioche_serialize) = brioche_serialize {
        let value = brioche_serialize
            .call(js_runtime, &value.value)?
            .maybe_await(js_runtime)
            .await?;

        if let Some(recipe) = cached_recipes.get(&value.value).copied() {
            cached_recipes.extend(equivalent_values.into_iter().map(|value| (value, recipe)));
            return Ok(recipe);
        }
        equivalent_values.push(value.value.clone());

        value
    } else if value.path.is_top_level() {
        return Err(DeserializeError::new(
            JsRuntimeError::MissingField,
            value.path,
        ));
    } else {
        value
    };

    let recipe = Box::pin(deserialize_recipe_value(
        brioche,
        js_runtime,
        value,
        module_namespace,
        cached_recipes,
    ))
    .await?;
    cached_recipes.extend(equivalent_values.into_iter().map(|value| (value, recipe)));

    Ok(recipe)
}

#[expect(clippy::mutable_key_type)]
async fn deserialize_recipe_value(
    brioche: &crate::Brioche,
    js_runtime: &mut deno_core::JsRuntime,
    value: ValueScope,
    module_namespace: &deno_core::v8::Global<deno_core::v8::Value>,
    cached_recipes: &mut HashMap<deno_core::v8::Global<deno_core::v8::Value>, RecipeRef>,
) -> Result<RecipeRef, DeserializeError> {
    let (value, kind) = value.get_tag::<RecipeKind>(js_runtime, "type")?;
    let recipe = match kind {
        RecipeKind::File => todo!(),
        RecipeKind::Directory => {
            let entry_values = value
                .get_field(js_runtime, "entries")?
                .into_object(js_runtime)?;

            if let Some((entry_name, _)) = entry_values.iter().next() {
                let path = value.path.clone().with_field(entry_name.clone());
                return Err(DeserializeError::new(
                    JsRuntimeError::InvalidValue {
                        reason: "unsupported directory entry value".into(),
                    },
                    path,
                ));
            }

            let entries = BTreeMap::new();
            Recipe::Directory(crate::recipe::Directory { entries })
        }
        RecipeKind::Symlink => {
            let target = value
                .get_field(js_runtime, "target")?
                .deserialize_tick_encoding(js_runtime)?;
            Recipe::Symlink(crate::recipe::Symlink { target })
        }
        RecipeKind::Download => {
            let url = value.get_field(js_runtime, "url")?;
            let url = url
                .to_string(js_runtime)?
                .parse()
                .map_err(|error| DeserializeError::new(error, url.path.clone()))?;

            let hash = value.get_field(js_runtime, "hash")?;
            let hash = deserialize_hash(js_runtime, hash)?;

            Recipe::Download(crate::recipe::DownloadRecipe { url, hash })
        }
        RecipeKind::Unarchive => {
            let file = value.get_field(js_runtime, "file")?;
            let file =
                deserialize_recipe(brioche, js_runtime, file, module_namespace, cached_recipes)
                    .await?;

            let archive = value
                .get_field(js_runtime, "archive")?
                .deserialize_value(js_runtime)?;

            let compression = value
                .get_field_or_nullish(js_runtime, "compression")?
                .map(|compression| compression.deserialize_value(js_runtime))
                .transpose()?
                .unwrap_or(crate::recipe::CompressionFormat::None);

            Recipe::Unarchive(crate::recipe::UnarchiveRecipe {
                file,
                archive,
                compression,
            })
        }
        RecipeKind::Process => {
            let command = value.get_field(js_runtime, "command")?;

            let args_values = value
                .get_field(js_runtime, "args")?
                .into_array(js_runtime)?;
            let mut args = vec![];
            for arg in args_values {
                let arg = deserialize_process_template(
                    brioche,
                    js_runtime,
                    arg,
                    module_namespace,
                    cached_recipes,
                )
                .await?;
                args.push(arg);
            }

            let mut env = BTreeMap::new();
            let env_values = value
                .get_field(js_runtime, "env")?
                .into_object(js_runtime)?;
            for (env_var, env_value) in env_values {
                let env_var = tick_encoding::decode(env_var.as_bytes()).map_err(|error| {
                    DeserializeError::new(error, value.path.clone().with_field("env"))
                })?;
                let env_var = bstr::BString::new(env_var.into_owned());

                let env_value = deserialize_process_template(
                    brioche,
                    js_runtime,
                    env_value,
                    module_namespace,
                    cached_recipes,
                )
                .await?;

                env.insert(env_var, env_value);
            }

            let current_dir = value.get_field_or_nullish(js_runtime, "currentDir")?;
            let current_dir = if let Some(current_dir) = current_dir {
                deserialize_process_template(
                    brioche,
                    js_runtime,
                    current_dir,
                    module_namespace,
                    cached_recipes,
                )
                .await?
            } else {
                crate::recipe::ProcessTemplate::default_current_dir()
            };

            let dependencies = value.get_field_or_nullish(js_runtime, "dependencies")?;
            let dependencies = if let Some(dependency_values) = dependencies {
                let dependency_values = dependency_values.into_array(js_runtime)?;
                let mut dependencies = vec![];

                for item in dependency_values {
                    let dependency = deserialize_recipe(
                        brioche,
                        js_runtime,
                        item,
                        module_namespace,
                        cached_recipes,
                    )
                    .await?;
                    dependencies.push(dependency);
                }

                dependencies
            } else {
                vec![]
            };

            let work_dir = value.get_field(js_runtime, "workDir")?;
            let work_dir = deserialize_recipe(
                brioche,
                js_runtime,
                work_dir,
                module_namespace,
                cached_recipes,
            )
            .await?;

            let output_scaffold = value.get_field_or_nullish(js_runtime, "outputScaffold")?;
            let output_scaffold = if let Some(output_scaffold) = output_scaffold {
                Some(
                    deserialize_recipe(
                        brioche,
                        js_runtime,
                        output_scaffold,
                        module_namespace,
                        cached_recipes,
                    )
                    .await?,
                )
            } else {
                None
            };

            Recipe::Process(crate::recipe::ProcessRecipe {
                command: deserialize_process_template(
                    brioche,
                    js_runtime,
                    command,
                    module_namespace,
                    cached_recipes,
                )
                .await?,
                args,
                env,
                current_dir,
                dependencies,
                work_dir,
                output_scaffold,
                platform: value
                    .get_field(js_runtime, "platform")?
                    .deserialize_value(js_runtime)?,
                is_unsafe: value
                    .get_field_or_nullish(js_runtime, "isUnsafe")?
                    .map(|value| value.deserialize_value(js_runtime))
                    .transpose()?
                    .unwrap_or(false),
                networking: value
                    .get_field_or_nullish(js_runtime, "networking")?
                    .map(|value| value.deserialize_value(js_runtime))
                    .transpose()?
                    .unwrap_or(false),
            })
        }
        RecipeKind::CompleteProcess => todo!(),
        RecipeKind::CreateFile => {
            let resources = value.get_field_or_nullish(js_runtime, "resources")?;
            let resources = if let Some(resources) = resources {
                Some(
                    deserialize_recipe(
                        brioche,
                        js_runtime,
                        resources,
                        module_namespace,
                        cached_recipes,
                    )
                    .await?,
                )
            } else {
                None
            };

            Recipe::CreateFile {
                content: value
                    .get_field(js_runtime, "content")?
                    .deserialize_tick_encoding(js_runtime)?,
                executable: value
                    .get_field(js_runtime, "executable")?
                    .deserialize_value(js_runtime)?,
                resources,
            }
        }
        RecipeKind::CreateDirectory => {
            let entry_values = value
                .get_field(js_runtime, "entries")?
                .into_object(js_runtime)?;

            let mut entries = BTreeMap::new();
            for (entry_name, entry_value) in entry_values {
                let entry_name = tick_encoding::decode(entry_name.as_bytes()).map_err(|error| {
                    DeserializeError::new(error, value.path.clone().with_field("entries"))
                })?;
                let entry_name = bstr::BString::new(entry_name.into_owned());

                let entry_value = deserialize_recipe(
                    brioche,
                    js_runtime,
                    entry_value,
                    module_namespace,
                    cached_recipes,
                )
                .await?;

                entries.insert(entry_name, entry_value);
            }

            Recipe::CreateDirectory { entries }
        }
        RecipeKind::Cast => {
            let recipe = value.get_field(js_runtime, "recipe")?;
            let recipe = deserialize_recipe(
                brioche,
                js_runtime,
                recipe,
                module_namespace,
                cached_recipes,
            )
            .await?;

            Recipe::Cast {
                recipe,
                to: value
                    .get_field(js_runtime, "to")?
                    .deserialize_value(js_runtime)?,
            }
        }
        RecipeKind::Merge => {
            let directory_values = value
                .get_field(js_runtime, "directories")?
                .into_array(js_runtime)?;
            let mut directories = vec![];

            for directory in directory_values {
                let directory = deserialize_recipe(
                    brioche,
                    js_runtime,
                    directory,
                    module_namespace,
                    cached_recipes,
                )
                .await?;
                directories.push(directory);
            }

            Recipe::Merge { directories }
        }
        RecipeKind::Peel => {
            let directory = value.get_field(js_runtime, "directory")?;
            let directory = deserialize_recipe(
                brioche,
                js_runtime,
                directory,
                module_namespace,
                cached_recipes,
            )
            .await?;

            let depth = value
                .get_field(js_runtime, "depth")?
                .deserialize_value(js_runtime)?;

            Recipe::Peel { directory, depth }
        }
        RecipeKind::Get => {
            let directory = value.get_field(js_runtime, "directory")?;
            let directory = deserialize_recipe(
                brioche,
                js_runtime,
                directory,
                module_namespace,
                cached_recipes,
            )
            .await?;

            let path = value
                .get_field(js_runtime, "path")?
                .deserialize_tick_encoding(js_runtime)?;

            Recipe::Get { directory, path }
        }
        RecipeKind::Insert => {
            let directory = value.get_field(js_runtime, "directory")?;
            let directory = deserialize_recipe(
                brioche,
                js_runtime,
                directory,
                module_namespace,
                cached_recipes,
            )
            .await?;

            let path = value
                .get_field(js_runtime, "path")?
                .deserialize_tick_encoding(js_runtime)?;

            let recipe = value.get_field_or_nullish(js_runtime, "recipe")?;
            let recipe = if let Some(recipe) = recipe {
                Some(
                    deserialize_recipe(
                        brioche,
                        js_runtime,
                        recipe,
                        module_namespace,
                        cached_recipes,
                    )
                    .await?,
                )
            } else {
                None
            };

            Recipe::Insert {
                directory,
                path,
                recipe,
            }
        }
        RecipeKind::Glob => todo!(),
        RecipeKind::SetPermissions => {
            let file = value.get_field(js_runtime, "file")?;
            let file =
                deserialize_recipe(brioche, js_runtime, file, module_namespace, cached_recipes)
                    .await?;

            let executable = value
                .get_field_or_nullish(js_runtime, "executable")?
                .map(|executable| executable.deserialize_value(js_runtime))
                .transpose()?;

            Recipe::SetPermissions { file, executable }
        }
        RecipeKind::CollectReferences => todo!(),
        RecipeKind::AttachResources => todo!(),
        RecipeKind::Proxy => todo!(),
        RecipeKind::Sync => todo!(),
    };

    let mut brioche = brioche.write().await;
    Ok(brioche.recipes.insert_recipe(Arc::new(recipe)))
}

#[expect(clippy::mutable_key_type)]
async fn deserialize_process_template(
    brioche: &crate::Brioche,
    js_runtime: &mut deno_core::JsRuntime,
    value: ValueScope,
    module_namespace: &deno_core::v8::Global<deno_core::v8::Value>,
    cached_recipes: &mut HashMap<deno_core::v8::Global<deno_core::v8::Value>, RecipeRef>,
) -> Result<crate::recipe::ProcessTemplate, DeserializeError> {
    let component_values = value
        .get_field(js_runtime, "components")?
        .into_array(js_runtime)?;
    let mut components = vec![];

    for component in component_values {
        let component = deserialize_process_template_component(
            brioche,
            js_runtime,
            component,
            module_namespace,
            cached_recipes,
        )
        .await?;
        components.push(component);
    }

    Ok(crate::recipe::ProcessTemplate { components })
}

#[expect(clippy::mutable_key_type)]
async fn deserialize_process_template_component(
    brioche: &crate::Brioche,
    js_runtime: &mut deno_core::JsRuntime,
    value: ValueScope,
    module_namespace: &deno_core::v8::Global<deno_core::v8::Value>,
    cached_recipes: &mut HashMap<deno_core::v8::Global<deno_core::v8::Value>, RecipeRef>,
) -> Result<crate::recipe::ProcessTemplateComponent, DeserializeError> {
    let (value, kind) = value.get_tag::<ProcessTemplateComponentKind>(js_runtime, "type")?;
    match kind {
        ProcessTemplateComponentKind::Literal => {
            let value = value.get_field(js_runtime, "value")?;
            let value = value.deserialize_tick_encoding(js_runtime)?;
            Ok(crate::recipe::ProcessTemplateComponent::Literal { value })
        }
        ProcessTemplateComponentKind::Input => {
            let recipe = value.get_field(js_runtime, "recipe")?;
            let recipe = deserialize_recipe(
                brioche,
                js_runtime,
                recipe,
                module_namespace,
                cached_recipes,
            )
            .await?;
            Ok(crate::recipe::ProcessTemplateComponent::Input { recipe })
        }
        ProcessTemplateComponentKind::OutputPath => {
            Ok(crate::recipe::ProcessTemplateComponent::OutputPath)
        }
        ProcessTemplateComponentKind::ResourceDir => {
            Ok(crate::recipe::ProcessTemplateComponent::ResourceDir)
        }
        ProcessTemplateComponentKind::InputResourceDirs => {
            Ok(crate::recipe::ProcessTemplateComponent::InputResourceDirs)
        }
        ProcessTemplateComponentKind::HomeDir => {
            Ok(crate::recipe::ProcessTemplateComponent::HomeDir)
        }
        ProcessTemplateComponentKind::WorkDir => {
            Ok(crate::recipe::ProcessTemplateComponent::WorkDir)
        }
        ProcessTemplateComponentKind::TempDir => {
            Ok(crate::recipe::ProcessTemplateComponent::TempDir)
        }
        ProcessTemplateComponentKind::CaCertificateBundlePath => {
            Ok(crate::recipe::ProcessTemplateComponent::CaCertificateBundlePath)
        }
    }
}

fn deserialize_hash(
    js_runtime: &mut deno_core::JsRuntime,
    value: ValueScope,
) -> Result<crate::hash::AnyHash, DeserializeError> {
    let (value, kind) = value.get_tag::<AnyHashKind>(js_runtime, "type")?;
    match kind {
        AnyHashKind::Sha256 => {
            let value = value.get_field(js_runtime, "value")?;
            let value = value
                .to_string(js_runtime)?
                .parse()
                .map_err(|error| DeserializeError::new(error, value.path.clone()))?;
            Ok(crate::hash::AnyHash::Sha256 { value })
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct ValueScope {
    value: deno_core::v8::Global<deno_core::v8::Value>,
    path: ValuePath,
}

impl ValueScope {
    pub(super) const fn new(
        value: deno_core::v8::Global<deno_core::v8::Value>,
        path: ValuePath,
    ) -> Self {
        Self { value, path }
    }

    fn deserialize_value<T>(
        self,
        js_runtime: &mut deno_core::JsRuntime,
    ) -> Result<T, DeserializeError>
    where
        T: DeserializeV8,
    {
        deno_core::scope!(js_scope, js_runtime);
        deno_core::v8::tc_scope!(let js_scope, js_scope);
        let value = deno_core::v8::Local::new(js_scope, self.value);
        T::deserialize(js_scope, value, &self.path)
    }

    fn deserialize_tick_encoding(
        self,
        js_runtime: &mut deno_core::JsRuntime,
    ) -> Result<bstr::BString, DeserializeError> {
        deno_core::scope!(js_scope, js_runtime);
        deno_core::v8::tc_scope!(let js_scope, js_scope);
        let value = deno_core::v8::Local::new(js_scope, self.value);
        let value =
            deno_core::v8::Local::<deno_core::v8::String>::try_from(value).map_err(|_| {
                DeserializeError::type_error("string", value.type_repr(), self.path.clone())
            })?;
        let string = value.to_rust_string_lossy(js_scope);
        let mut bytes = string.into_bytes();
        let decoded = tick_encoding::decode_in_place(&mut bytes)
            .map_err(|error| DeserializeError::new(error, self.path.clone()))?;
        let decoded_len = decoded.len();
        bytes.truncate(decoded_len);
        Ok(bstr::BString::new(bytes))
    }

    fn try_get_field(
        &self,
        js_runtime: &mut deno_core::JsRuntime,
        key: impl Into<Cow<'static, str>>,
    ) -> Result<Option<Self>, DeserializeError> {
        deno_core::scope!(js_scope, js_runtime);
        deno_core::v8::tc_scope!(let js_scope, js_scope);

        let key = key.into();
        let key_string = deno_core::v8::String::new(js_scope, &key).ok_or_else(|| {
            DeserializeError::new(
                JsRuntimeError::InvalidJsString(key.clone()),
                self.path.clone(),
            )
        })?;

        let value = deno_core::v8::Local::new(js_scope, self.value.clone());
        let value =
            deno_core::v8::Local::<deno_core::v8::Object>::try_from(value).map_err(|_| {
                DeserializeError::type_error("object", value.type_repr(), self.path.clone())
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
            path: self.path.clone().with_field(key),
        }))
    }

    fn get_field_or_nullish(
        &self,
        js_runtime: &mut deno_core::JsRuntime,
        key: impl Into<Cow<'static, str>>,
    ) -> Result<Option<Self>, DeserializeError> {
        deno_core::scope!(js_scope, js_runtime);
        deno_core::v8::tc_scope!(let js_scope, js_scope);

        let key = key.into();
        let key_string = deno_core::v8::String::new(js_scope, &key).ok_or_else(|| {
            DeserializeError::new(
                JsRuntimeError::InvalidJsString(key.clone()),
                self.path.clone(),
            )
        })?;

        let value = deno_core::v8::Local::new(js_scope, self.value.clone());
        let value =
            deno_core::v8::Local::<deno_core::v8::Object>::try_from(value).map_err(|_| {
                DeserializeError::type_error("object", value.type_repr(), self.path.clone())
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
            path: self.path.clone().with_field(key),
        }))
    }

    fn get_field(
        &self,
        js_runtime: &mut deno_core::JsRuntime,
        key: impl Into<Cow<'static, str>>,
    ) -> Result<Self, DeserializeError> {
        let key = key.into();
        let path = self.path.clone();
        let field = self
            .try_get_field(js_runtime, key.clone())?
            .ok_or_else(|| {
                DeserializeError::new(JsRuntimeError::MissingField, path.with_field(key))
            })?;
        Ok(field)
    }

    fn get_tag<T>(
        self,
        js_runtime: &mut deno_core::JsRuntime,
        tag_key: impl Into<Cow<'static, str>>,
    ) -> Result<(Self, T), DeserializeError>
    where
        T: JsEnumTag,
    {
        deno_core::scope!(js_scope, js_runtime);
        deno_core::v8::tc_scope!(let js_scope, js_scope);

        let tag_key = tag_key.into();
        let tag_key_string = deno_core::v8::String::new(js_scope, &tag_key).ok_or_else(|| {
            DeserializeError::new(
                JsRuntimeError::InvalidJsString(tag_key.clone()),
                self.path.clone(),
            )
        })?;

        let value = deno_core::v8::Local::new(js_scope, self.value.clone());
        let value =
            deno_core::v8::Local::<deno_core::v8::Object>::try_from(value).map_err(|_| {
                DeserializeError::type_error("object", value.type_repr(), self.path.clone())
            })?;

        let tag_value = value.get(js_scope, tag_key_string.into());
        let Some(tag_value) = tag_value else {
            return Err(DeserializeError::new(
                JsRuntimeError::MissingField,
                self.path.clone().with_field(tag_key),
            ));
        };
        let tag = T::deserialize(js_scope, tag_value, &self.path.clone().with_field(tag_key))?;

        Ok((
            Self {
                value: self.value,
                path: self.path.with_variant(T::tag(&tag)),
            },
            tag,
        ))
    }

    fn into_array(
        self,
        js_runtime: &mut deno_core::JsRuntime,
    ) -> Result<Vec<Self>, DeserializeError> {
        deno_core::scope!(js_scope, js_runtime);

        let value = deno_core::v8::Local::new(js_scope, &self.value);
        let value =
            deno_core::v8::Local::<deno_core::v8::Array>::try_from(value).map_err(|_| {
                DeserializeError::type_error("array", value.type_repr(), self.path.clone())
            })?;

        let mut items = vec![];
        for i in 0..value.length() {
            let path = self.path.clone().with_index(i);
            let item = value.get_index(js_scope, i).ok_or_else(|| {
                DeserializeError::new(
                    JsRuntimeError::InvalidValue {
                        reason: "index not set".into(),
                    },
                    path.clone(),
                )
            })?;
            let item = deno_core::v8::Global::new(js_scope, item);
            items.push(Self { value: item, path });
        }

        Ok(items)
    }

    fn into_object(
        self,
        js_runtime: &mut deno_core::JsRuntime,
    ) -> Result<HashMap<String, Self>, DeserializeError> {
        deno_core::scope!(js_scope, js_runtime);

        let value = deno_core::v8::Local::new(js_scope, &self.value);
        let value =
            deno_core::v8::Local::<deno_core::v8::Object>::try_from(value).map_err(|_| {
                DeserializeError::type_error("object", value.type_repr(), self.path.clone())
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
                    self.path.clone(),
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
                        self.path.clone(),
                    )
                })?;
            let property_string = property_string.to_rust_string_lossy(js_scope);

            let path = self.path.clone().with_field(property_string.clone());

            let value = value.get(js_scope, property).ok_or_else(|| {
                DeserializeError::new(
                    JsRuntimeError::InvalidValue {
                        reason: "property does not exist in object".into(),
                    },
                    path.clone(),
                )
            })?;
            let value = deno_core::v8::Global::new(js_scope, value);
            let value = Self { value, path };

            entries.insert(property_string, value);
        }

        Ok(entries)
    }

    fn to_string(&self, js_runtime: &mut deno_core::JsRuntime) -> Result<String, DeserializeError> {
        deno_core::scope!(js_scope, js_runtime);

        let value = deno_core::v8::Local::new(js_scope, self.value.clone());
        let string =
            deno_core::v8::Local::<deno_core::v8::String>::try_from(value).map_err(|_| {
                DeserializeError::type_error("string", value.type_repr(), self.path.clone())
            })?;
        Ok(string.to_rust_string_lossy(js_scope))
    }

    fn call(
        self,
        js_runtime: &mut deno_core::JsRuntime,
        this: &deno_core::v8::Global<deno_core::v8::Value>,
    ) -> Result<Self, DeserializeError> {
        deno_core::scope!(js_scope, js_runtime);
        deno_core::v8::tc_scope!(let js_scope, js_scope);

        let value = deno_core::v8::Local::new(js_scope, &self.value);
        let function =
            deno_core::v8::Local::<deno_core::v8::Function>::try_from(value).map_err(|_| {
                DeserializeError::type_error("function", value.type_repr(), self.path.clone())
            })?;

        let path = self.path.with_call();
        let this = deno_core::v8::Local::new(js_scope, this);
        let value = function.call(js_scope, this, &[]);
        let Some(value) = value else {
            if let Some(exception) = js_scope.exception() {
                return Err(DeserializeError::new(
                    deno_core::error::JsError::from_v8_exception(js_scope, exception),
                    path,
                ));
            }
            return Err(DeserializeError::new(
                JsRuntimeError::UnknownEvalError {
                    reason: "function call failed without an exception".into(),
                },
                path,
            ));
        };

        Ok(Self {
            value: deno_core::v8::Global::new(js_scope, value),
            path,
        })
    }

    fn maybe_call(
        self,
        js_runtime: &mut deno_core::JsRuntime,
        this: &deno_core::v8::Global<deno_core::v8::Value>,
    ) -> Result<Self, DeserializeError> {
        deno_core::scope!(js_scope, js_runtime);
        deno_core::v8::tc_scope!(let js_scope, js_scope);

        let value = deno_core::v8::Local::new(js_scope, &self.value);
        let function = deno_core::v8::Local::<deno_core::v8::Function>::try_from(value);
        let Ok(function) = function else {
            return Ok(self);
        };

        let path = self.path.with_call();
        let this = deno_core::v8::Local::new(js_scope, this);
        let value = function.call(js_scope, this, &[]);
        let Some(value) = value else {
            if let Some(exception) = js_scope.exception() {
                return Err(DeserializeError::new(
                    deno_core::error::JsError::from_v8_exception(js_scope, exception),
                    path,
                ));
            }
            return Err(DeserializeError::new(
                JsRuntimeError::UnknownEvalError {
                    reason: "function call failed without an exception".into(),
                },
                path,
            ));
        };

        Ok(Self {
            value: deno_core::v8::Global::new(js_scope, value),
            path,
        })
    }

    async fn maybe_await(
        self,
        js_runtime: &mut deno_core::JsRuntime,
    ) -> Result<Self, DeserializeError> {
        let value_fut = js_runtime.resolve(self.value);
        let value = js_runtime
            .with_event_loop_promise(value_fut, deno_core::PollEventLoopOptions::default())
            .await
            .map_err(|error| DeserializeError::new(error, self.path.clone()))?;
        Ok(Self {
            value,
            path: self.path,
        })
    }
}

trait DeserializeV8: Sized {
    fn deserialize(
        js_scope: &mut deno_core::v8::PinScope,
        value: deno_core::v8::Local<deno_core::v8::Value>,
        path: &ValuePath,
    ) -> Result<Self, DeserializeError>;
}

impl DeserializeV8 for bool {
    fn deserialize(
        js_scope: &mut deno_core::v8::PinScope,
        value: deno_core::v8::Local<deno_core::v8::Value>,
        path: &ValuePath,
    ) -> Result<Self, DeserializeError> {
        let value =
            deno_core::v8::Local::<deno_core::v8::Boolean>::try_from(value).map_err(|_| {
                DeserializeError::type_error("boolean", value.type_repr(), path.clone())
            })?;
        Ok(value.boolean_value(js_scope))
    }
}

impl DeserializeV8 for f64 {
    fn deserialize(
        _js_scope: &mut deno_core::v8::PinScope,
        value: deno_core::v8::Local<deno_core::v8::Value>,
        path: &ValuePath,
    ) -> Result<Self, DeserializeError> {
        let value = deno_core::v8::Local::<deno_core::v8::Number>::try_from(value)
            .map_err(|_| DeserializeError::type_error("number", value.type_repr(), path.clone()))?;
        Ok(value.value())
    }
}

impl DeserializeV8 for i64 {
    #[expect(clippy::cast_possible_truncation, clippy::float_cmp)]
    fn deserialize(
        js_scope: &mut deno_core::v8::PinScope,
        value: deno_core::v8::Local<deno_core::v8::Value>,
        path: &ValuePath,
    ) -> Result<Self, DeserializeError> {
        let number = f64::deserialize(js_scope, value, path)?;
        let rounded = number.round();
        if rounded != number {
            return Err(DeserializeError::new(
                JsRuntimeError::InvalidValue {
                    reason: "number is not an integer".into(),
                },
                path.clone(),
            ));
        }

        Ok(rounded as Self)
    }
}

impl DeserializeV8 for u32 {
    fn deserialize(
        js_scope: &mut deno_core::v8::PinScope,
        value: deno_core::v8::Local<deno_core::v8::Value>,
        path: &ValuePath,
    ) -> Result<Self, DeserializeError> {
        let number = i64::deserialize(js_scope, value, path)?;
        let number =
            Self::try_from(number).map_err(|error| DeserializeError::new(error, path.clone()))?;
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
        path: &ValuePath,
    ) -> Result<Self, DeserializeError> {
        let string = deno_core::v8::Local::<deno_core::v8::String>::try_from(value)
            .map_err(|_| DeserializeError::type_error("string", value.type_repr(), path.clone()))?;
        let string = deno_core::v8::ValueView::new(js_scope, string);
        let string = string.to_cow_lossy();
        let value = T::from_str(&string).ok_or_else(|| {
            DeserializeError::new(
                JsRuntimeError::InvalidEnumVariant {
                    expected: T::VALUES.iter().map(T::tag).collect(),
                    got: string.into_owned(),
                },
                path.clone(),
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
pub struct ValuePath {
    #[expect(unused)]
    module: ModuleRef,
    #[expect(unused)]
    module_path: AbsolutePath,
    export: String,
    components: Vec<ValuePathComponent>,
}

impl ValuePath {
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

    fn with_field(mut self, field: impl Into<Cow<'static, str>>) -> Self {
        self.components
            .push(ValuePathComponent::Field(field.into()));
        self
    }

    fn with_index(mut self, index: u32) -> Self {
        self.components.push(ValuePathComponent::Index(index));
        self
    }

    fn with_variant(mut self, variant: impl Into<Cow<'static, str>>) -> Self {
        self.components
            .push(ValuePathComponent::Variant(variant.into()));
        self
    }

    fn with_call(mut self) -> Self {
        self.components.push(ValuePathComponent::Call);
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
                    ValuePathComponent::Call => "()",
                    ValuePathComponent::Variant(variant) => "<{variant}>",
                    ValuePathComponent::Field(field) if is_safe_field(field) => ".{field}",
                    ValuePathComponent::Field(field) => "['{field}']",
                    ValuePathComponent::Index(index) => "[{index}]",
                })
            })
            .join_concat();
        lazy_format::lazy_format!("{}{}", self.export, components)
    }
}

#[derive(Debug, Clone)]
enum ValuePathComponent {
    Call,
    Variant(Cow<'static, str>),
    Field(Cow<'static, str>),
    Index(u32),
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct DeserializeError(Box<DeserializeErrorInner>);

impl DeserializeError {
    fn new<E>(error: E, path: ValuePath) -> Self
    where
        E: Into<JsRuntimeError>,
    {
        Self(Box::new(DeserializeErrorInner {
            path,
            error: error.into(),
        }))
    }

    fn type_error(expected: &'static str, actual: &'static str, path: ValuePath) -> Self {
        Self(Box::new(DeserializeErrorInner {
            path,
            error: JsRuntimeError::TypeError {
                expected: Cow::Borrowed(expected),
                actual: actual.into(),
            },
        }))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("error deserializing {}: {error}", .path.display_pretty())]
struct DeserializeErrorInner {
    path: ValuePath,
    error: JsRuntimeError,
}
