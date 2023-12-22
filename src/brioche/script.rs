use std::{
    cell::RefCell,
    collections::{hash_map::Entry, HashMap},
    rc::Rc,
};

use anyhow::Context as _;
use deno_core::OpState;

use self::specifier::BriocheModuleSpecifier;

use super::{
    project::Project,
    value::{CompleteValue, LazyValue, WithMeta},
    Brioche,
};

pub mod check;
mod compiler_host;
pub mod evaluate;
mod js;
pub mod lsp;
pub mod specifier;

#[derive(Clone)]
struct BriocheModuleLoader {
    pub brioche: Brioche,
    pub sources: Rc<RefCell<HashMap<BriocheModuleSpecifier, ModuleSource>>>,
}

impl BriocheModuleLoader {
    fn new(brioche: &Brioche) -> Self {
        Self {
            brioche: brioche.clone(),
            sources: Rc::new(RefCell::new(HashMap::new())),
        }
    }
}

impl deno_core::ModuleLoader for BriocheModuleLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        kind: deno_core::ResolutionKind,
    ) -> Result<deno_core::ModuleSpecifier, anyhow::Error> {
        if let deno_core::ResolutionKind::MainModule = kind {
            let resolved = specifier.parse()?;
            tracing::debug!(%specifier, %referrer, %resolved, "resolved main module");
            return Ok(resolved);
        }

        let referrer: BriocheModuleSpecifier = referrer.parse()?;
        let resolved = specifier::resolve(&self.brioche, specifier, &referrer)?;

        tracing::debug!(%specifier, %referrer, %resolved, "resolved module");

        let resolved: deno_core::ModuleSpecifier = resolved.into();
        Ok(resolved)
    }

    fn load(
        &self,
        module_specifier: &deno_core::ModuleSpecifier,
        _maybe_referrer: Option<&deno_core::ModuleSpecifier>,
        _is_dyn_import: bool,
    ) -> std::pin::Pin<Box<deno_core::ModuleSourceFuture>> {
        let module_specifier: Result<BriocheModuleSpecifier, _> = module_specifier.try_into();
        let sources = self.sources.clone();
        let future = async move {
            let module_specifier = module_specifier?;
            let Some(mut reader) = specifier::read_specifier_contents_sync(&module_specifier)?
            else {
                anyhow::bail!("file not found for module {module_specifier}");
            };

            let mut code = String::new();
            reader.read_to_string(&mut code)?;

            let parsed = deno_ast::parse_module(deno_ast::ParseParams {
                specifier: module_specifier.to_string(),
                text_info: deno_ast::SourceTextInfo::from_string(code.clone()),
                media_type: deno_ast::MediaType::TypeScript,
                capture_tokens: false,
                scope_analysis: false,
                maybe_syntax: None,
            })?;
            let transpiled = parsed.transpile(&deno_ast::EmitOptions {
                source_map: true,
                inline_source_map: false,
                ..deno_ast::EmitOptions::default()
            })?;

            let mut sources = sources.borrow_mut();
            if let Entry::Vacant(entry) = sources.entry(module_specifier.clone()) {
                let source_map = transpiled.source_map.context("source map not generated")?;
                entry.insert(ModuleSource {
                    source_map: source_map.into_bytes(),
                    source_text: code.clone(),
                });
            }

            let module_specifier: deno_core::ModuleSpecifier = module_specifier.into();
            Ok(deno_core::ModuleSource::new(
                deno_core::ModuleType::JavaScript,
                transpiled.text.into(),
                &module_specifier,
            ))
        };

        Box::pin(future)
    }
}

impl deno_core::SourceMapGetter for BriocheModuleLoader {
    fn get_source_map(&self, file_name: &str) -> Option<Vec<u8>> {
        let sources = self.sources.borrow();
        let specifier: BriocheModuleSpecifier = file_name.parse().ok()?;
        let code = sources.get(&specifier)?;
        Some(code.source_map.clone())
    }

    fn get_source_line(&self, file_name: &str, line_number: usize) -> Option<String> {
        let sources = self.sources.borrow();
        let specifier: BriocheModuleSpecifier = file_name.parse().ok()?;
        let source = sources.get(&specifier)?;
        let line = source.source_text.lines().nth(line_number)?;
        Some(line.to_string())
    }
}

struct ModuleSource {
    pub source_text: String,
    pub source_map: Vec<u8>,
}

deno_core::extension!(brioche_rt,
    ops = [
        op_brioche_resolve_all,
        op_brioche_create_proxy,
    ],
    options = {
        brioche: Brioche,
    },
    state = |state, options| {
        state.put(options.brioche);
    },
);

#[deno_core::op]
pub async fn op_brioche_resolve_all(
    state: Rc<RefCell<OpState>>,
    values: Vec<WithMeta<LazyValue>>,
) -> anyhow::Result<Vec<CompleteValue>> {
    let brioche = {
        let state = state.try_borrow()?;
        state
            .try_borrow::<Brioche>()
            .context("failed to get brioche instance")?
            .clone()
    };

    let mut results = vec![];
    for value in values {
        let result = super::resolve::resolve(&brioche, value).await?;
        results.push(result.value);
    }
    Ok(results)
}

#[deno_core::op]
pub async fn op_brioche_create_proxy(
    state: Rc<RefCell<OpState>>,
    value: LazyValue,
) -> anyhow::Result<LazyValue> {
    let brioche = {
        let state = state.try_borrow()?;
        state
            .try_borrow::<Brioche>()
            .context("failed to get brioche instance")?
            .clone()
    };

    let result = super::resolve::create_proxy(&brioche, value).await;
    Ok(result)
}
