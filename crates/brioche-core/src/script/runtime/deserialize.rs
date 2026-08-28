use std::{borrow::Cow, collections::HashMap, sync::Arc};

use joinery::JoinableIterator as _;

use crate::{
    path::AbsolutePath,
    project::ModuleRef,
    recipe::{Recipe, RecipeKind, RecipeRef},
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
        RecipeKind::Directory => todo!(),
        RecipeKind::Symlink => todo!(),
        RecipeKind::Download => todo!(),
        RecipeKind::Unarchive => todo!(),
        RecipeKind::Process => todo!(),
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
        RecipeKind::CreateDirectory => todo!(),
        RecipeKind::Cast => todo!(),
        RecipeKind::Merge => todo!(),
        RecipeKind::Peel => todo!(),
        RecipeKind::Get => todo!(),
        RecipeKind::Insert => todo!(),
        RecipeKind::Glob => todo!(),
        RecipeKind::SetPermissions => todo!(),
        RecipeKind::CollectReferences => todo!(),
        RecipeKind::AttachResources => todo!(),
        RecipeKind::Proxy => todo!(),
        RecipeKind::Sync => todo!(),
    };

    let mut brioche = brioche.write().await;
    Ok(brioche.recipes.insert_recipe(Arc::new(recipe)))
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
        let tag_value = deno_core::v8::Local::<deno_core::v8::String>::try_from(tag_value)
            .map_err(|_| {
                DeserializeError::type_error(
                    "string",
                    tag_value.type_repr(),
                    self.path.clone().with_field(tag_key.clone()),
                )
            })?;
        let tag_string = deno_core::v8::ValueView::new(js_scope, tag_value);
        let tag = T::from_str(&tag_string.to_cow_lossy()).ok_or_else(|| {
            DeserializeError::new(
                JsRuntimeError::InvalidEnumVariant {
                    expected: T::VALUES.iter().map(T::tag).collect(),
                    got: tag_string.to_cow_lossy().into_owned(),
                },
                self.path.clone().with_field(tag_key.clone()),
            )
        })?;

        Ok((
            Self {
                value: self.value,
                path: self
                    .path
                    .with_variant(tag_string.to_cow_lossy().into_owned()),
            },
            tag,
        ))
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
