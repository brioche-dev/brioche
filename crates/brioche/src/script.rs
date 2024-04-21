use std::{
    cell::RefCell,
    collections::{hash_map::Entry, HashMap},
    rc::Rc,
    sync::Arc,
};

use anyhow::Context as _;
use deno_core::OpState;
use specifier::BriocheModuleSpecifier;

use crate::resolve::ResolveScope;

use super::{
    artifact::{CompleteArtifact, LazyArtifact, WithMeta},
    blob::BlobHash,
    project::Projects,
    script::specifier::BriocheImportSpecifier,
    Brioche,
};

pub mod check;
mod compiler_host;
pub mod evaluate;
pub mod format;
mod js;
pub mod lsp;
pub mod specifier;

#[derive(Clone)]
struct BriocheModuleLoader {
    pub brioche: Brioche,
    pub projects: Projects,
    pub sources: Rc<RefCell<HashMap<BriocheModuleSpecifier, ModuleSource>>>,
}

impl BriocheModuleLoader {
    fn new(brioche: &Brioche, projects: &Projects) -> Self {
        Self {
            brioche: brioche.clone(),
            projects: projects.clone(),
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
        let specifier: BriocheImportSpecifier = specifier.parse()?;
        let resolved = specifier::resolve(&self.projects, &specifier, &referrer)?;

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
        let vfs = self.brioche.vfs.clone();
        let future = async move {
            let module_specifier = module_specifier?;
            let contents = specifier::read_specifier_contents(&vfs, &module_specifier)?;

            let code = std::str::from_utf8(&contents)
                .context("failed to parse module contents as UTF-8 string")?;

            let parsed = deno_ast::parse_module(deno_ast::ParseParams {
                specifier: module_specifier.to_string(),
                text_info: deno_ast::SourceTextInfo::from_string(code.to_string()),
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
                    source_contents: contents.clone(),
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
        let code = std::str::from_utf8(&source.source_contents)
            .inspect_err(|err| {
                tracing::warn!("failed to parse source contents of {file_name} as UTF-8: {err}");
            })
            .ok()?;
        let line = code.lines().nth(line_number)?;
        Some(line.to_string())
    }
}

struct ModuleSource {
    pub source_contents: Arc<Vec<u8>>,
    pub source_map: Vec<u8>,
}

deno_core::extension!(brioche_rt,
    ops = [
        op_brioche_resolve_all,
        op_brioche_create_proxy,
        op_brioche_read_blob,
    ],
    options = {
        brioche: Brioche,
        resolve_scope: ResolveScope,
    },
    state = |state, options| {
        state.put(options.brioche);
        state.put(options.resolve_scope);
    },
);

#[deno_core::op]
pub async fn op_brioche_resolve_all(
    state: Rc<RefCell<OpState>>,
    artifacts: Vec<WithMeta<LazyArtifact>>,
) -> anyhow::Result<Vec<CompleteArtifact>> {
    let brioche = {
        let state = state.try_borrow()?;
        state
            .try_borrow::<Brioche>()
            .context("failed to get brioche instance")?
            .clone()
    };
    let resolve_scope = {
        let state = state.try_borrow()?;
        state
            .try_borrow::<ResolveScope>()
            .context("failed to get resolve scope")?
            .clone()
    };

    let mut results = vec![];
    for artifact in artifacts {
        let result = super::resolve::resolve(&brioche, artifact, &resolve_scope).await?;
        results.push(result.value);
    }
    Ok(results)
}

#[deno_core::op]
pub async fn op_brioche_create_proxy(
    state: Rc<RefCell<OpState>>,
    artifact: LazyArtifact,
) -> anyhow::Result<LazyArtifact> {
    let brioche = {
        let state = state.try_borrow()?;
        state
            .try_borrow::<Brioche>()
            .context("failed to get brioche instance")?
            .clone()
    };

    let result = super::resolve::create_proxy(&brioche, artifact).await?;
    Ok(result)
}

// TODO: Return a Uint8Array instead of tick-encoding
#[deno_core::op]
pub async fn op_brioche_read_blob(
    state: Rc<RefCell<OpState>>,
    blob_hash: BlobHash,
) -> anyhow::Result<crate::encoding::TickEncode<Vec<u8>>> {
    let brioche = {
        let state = state.try_borrow()?;
        state
            .try_borrow::<Brioche>()
            .context("failed to get brioche instance")?
            .clone()
    };

    let path = crate::blob::blob_path(&brioche, blob_hash).await?;
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("failed to read blob {blob_hash}"))?;

    Ok(crate::encoding::TickEncode(bytes))
}
