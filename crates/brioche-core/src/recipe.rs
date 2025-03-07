use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    sync::{Arc, OnceLock, RwLock},
};

use anyhow::Context as _;
use bstr::{BStr, BString, ByteSlice as _, ByteVec as _};
use futures::{StreamExt as _, TryStreamExt as _};
use joinery::JoinableIterator as _;
use sqlx::{Acquire as _, Arguments as _};
use wax::Pattern as _;

use crate::encoding::TickEncoded;

use super::{Brioche, Hash, blob::BlobHash, platform::Platform};

#[serde_with::serde_as]
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    strum::EnumDiscriminants,
)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
#[strum_discriminants(vis(pub))]
#[strum_discriminants(derive(serde::Serialize, serde::Deserialize))]
#[strum_discriminants(serde(rename_all = "snake_case"))]
pub enum Recipe {
    #[serde(rename_all = "camelCase")]
    File {
        content_blob: BlobHash,
        executable: bool,
        resources: Box<WithMeta<Recipe>>,
    },
    #[serde(rename_all = "camelCase")]
    Directory(Directory),
    #[serde(rename_all = "camelCase")]
    Symlink {
        #[serde_as(as = "TickEncoded")]
        target: BString,
    },
    #[serde(rename_all = "camelCase")]
    Download(DownloadRecipe),
    #[serde(rename_all = "camelCase")]
    Unarchive(Unarchive),
    Process(ProcessRecipe),
    CompleteProcess(CompleteProcessRecipe),
    #[serde(rename_all = "camelCase")]
    CreateFile {
        #[serde_as(as = "TickEncoded")]
        content: BString,
        executable: bool,
        resources: Box<WithMeta<Recipe>>,
    },
    #[serde(rename_all = "camelCase")]
    CreateDirectory(CreateDirectory),
    #[serde(rename_all = "camelCase")]
    Cast {
        recipe: Box<WithMeta<Recipe>>,
        to: ArtifactDiscriminants,
    },
    #[serde(rename_all = "camelCase")]
    Merge {
        directories: Vec<WithMeta<Recipe>>,
    },
    #[serde(rename_all = "camelCase")]
    Peel {
        directory: Box<WithMeta<Recipe>>,
        depth: u32,
    },
    #[serde(rename_all = "camelCase")]
    Get {
        directory: Box<WithMeta<Recipe>>,
        #[serde_as(as = "TickEncoded")]
        path: BString,
    },
    #[serde(rename_all = "camelCase")]
    Insert {
        directory: Box<WithMeta<Recipe>>,
        #[serde_as(as = "TickEncoded")]
        path: BString,
        recipe: Option<Box<WithMeta<Recipe>>>,
    },
    Glob {
        directory: Box<WithMeta<Recipe>>,
        patterns: BTreeSet<BString>,
    },
    #[serde(rename_all = "camelCase")]
    SetPermissions {
        file: Box<WithMeta<Recipe>>,
        executable: Option<bool>,
    },
    #[serde(rename_all = "camelCase")]
    CollectReferences {
        recipe: Box<WithMeta<Recipe>>,
    },
    #[serde(rename_all = "camelCase")]
    AttachResources {
        recipe: Box<WithMeta<Recipe>>,
    },
    #[serde(rename_all = "camelCase")]
    Proxy(ProxyRecipe),
    #[serde(rename_all = "camelCase")]
    Sync {
        recipe: Box<WithMeta<Recipe>>,
    },
}

impl Recipe {
    pub fn try_hash(&self) -> anyhow::Result<RecipeHash> {
        static HASHES: OnceLock<RwLock<HashMap<Recipe, RecipeHash>>> = OnceLock::new();
        let hashes = HASHES.get_or_init(|| RwLock::new(HashMap::new()));
        {
            let hashes_reader = hashes
                .read()
                .map_err(|_| anyhow::anyhow!("failed to acquire read lock on hashes"))?;
            if let Some(hash) = hashes_reader.get(self) {
                return Ok(*hash);
            }
        }

        let hash = RecipeHash::from_serializable(self)?;
        {
            let mut hashes_writer = hashes
                .write()
                .map_err(|_| anyhow::anyhow!("failed to acquire write lock on hashes"))?;
            hashes_writer.insert(self.clone(), hash);
        }

        Ok(hash)
    }

    pub fn hash(&self) -> RecipeHash {
        self.try_hash().expect("failed to hash recipe")
    }

    pub fn kind(&self) -> RecipeDiscriminants {
        self.into()
    }

    pub const fn is_expensive_to_bake(&self) -> bool {
        match self {
            Self::Download(_) | Self::Process(_) | Self::CompleteProcess(_) | Self::Sync { .. } => {
                true
            }
            Self::File { .. }
            | Self::Directory(_)
            | Self::Symlink { .. }
            | Self::Unarchive(_)
            | Self::CreateFile { .. }
            | Self::CreateDirectory(_)
            | Self::Cast { .. }
            | Self::Merge { .. }
            | Self::Peel { .. }
            | Self::Get { .. }
            | Self::Insert { .. }
            | Self::Glob { .. }
            | Self::SetPermissions { .. }
            | Self::CollectReferences { .. }
            | Self::AttachResources { .. }
            | Self::Proxy(_) => false,
        }
    }
}

pub async fn get_recipes(
    brioche: &Brioche,
    recipe_hashes: impl IntoIterator<Item = RecipeHash>,
) -> anyhow::Result<HashMap<RecipeHash, Recipe>> {
    let mut recipes = HashMap::new();

    let cached_recipes = brioche.cached_recipes.read().await;
    let mut uncached_recipes = HashSet::new();

    for recipe_hash in recipe_hashes {
        match cached_recipes.recipes_by_hash.get(&recipe_hash) {
            Some(recipe) => {
                recipes.insert(recipe_hash, recipe.clone());
            }
            None => {
                uncached_recipes.insert(recipe_hash);
            }
        }
    }

    // Release the lock
    drop(cached_recipes);

    // Return early if we have no uncached recipes to fetch
    if uncached_recipes.is_empty() {
        return Ok(recipes);
    }

    let mut db_conn = brioche.db_conn.lock().await;
    let mut db_transaction = db_conn.begin().await?;

    let mut records = vec![];
    {
        let uncached_recipes = uncached_recipes.iter().copied().collect::<Vec<_>>();

        // Fetch recipes in batches to avoid hitting the maximum number of
        // SQLite variables per query
        for uncached_recipe_batch in uncached_recipes.chunks(900) {
            let mut arguments = sqlx::sqlite::SqliteArguments::default();

            for recipe_hash in uncached_recipe_batch {
                arguments
                    .add(recipe_hash.to_string())
                    .map_err(|error| anyhow::anyhow!(error))?;
            }

            let placeholders = std::iter::repeat("?")
                .take(uncached_recipe_batch.len())
                .join_with(", ");

            let batch_records = sqlx::query_as_with::<_, (String, String), _>(
                &format!(
                    r#"
                        SELECT recipe_hash, recipe_json
                        FROM recipes
                        WHERE recipe_hash IN ({placeholders})
                    "#,
                ),
                arguments,
            )
            .fetch_all(&mut *db_transaction)
            .await?;

            records.extend(batch_records);
        }
    }

    db_transaction.commit().await?;
    drop(db_conn);

    let mut cached_recipes = brioche.cached_recipes.write().await;
    for (recipe_hash, recipe_json) in records {
        let recipe: Recipe = serde_json::from_str(&recipe_json)?;
        let expected_recipe_hash: RecipeHash = recipe_hash.parse()?;
        let recipe_hash = recipe.hash();

        anyhow::ensure!(
            expected_recipe_hash == recipe_hash,
            "expected recipe hash from database to be {expected_recipe_hash}, but was {recipe_hash}"
        );

        cached_recipes
            .recipes_by_hash
            .insert(recipe_hash, recipe.clone());
        uncached_recipes.remove(&recipe_hash);
        recipes.insert(recipe_hash, recipe);
    }

    if !uncached_recipes.is_empty() {
        anyhow::bail!("recipes not found: {uncached_recipes:?}");
    }

    Ok(recipes)
}

pub async fn get_recipe(brioche: &Brioche, recipe_hash: RecipeHash) -> anyhow::Result<Recipe> {
    let mut recipes = get_recipes(brioche, [recipe_hash]).await?;
    let recipe = recipes
        .remove(&recipe_hash)
        .expect("recipe not returned in collection");
    Ok(recipe)
}

pub async fn save_recipes<A>(
    brioche: &Brioche,
    recipes: impl IntoIterator<Item = A>,
) -> anyhow::Result<u64>
where
    A: std::borrow::Borrow<Recipe>,
{
    let cached_recipes = brioche.cached_recipes.read().await;

    let mut uncached_recipes = vec![];
    for recipe in recipes {
        let recipe = recipe.borrow();

        if !cached_recipes.recipes_by_hash.contains_key(&recipe.hash()) {
            // Recipe not cached, so try to insert it into the database
            // and cache it afterward
            uncached_recipes.push(recipe.clone());
        }
    }

    // Release the read lock
    drop(cached_recipes);

    // Short-circuit if we have no recipes to save
    if uncached_recipes.is_empty() {
        return Ok(0);
    }

    let mut db_conn = brioche.db_conn.lock().await;
    let mut db_transaction = db_conn.begin().await?;

    // Save each recipe to the database in batches, so we don't hit the
    // maximum number of variables per query
    let mut num_rows_affected = 0;
    for recipe_batch in uncached_recipes.chunks(400) {
        let mut arguments = sqlx::sqlite::SqliteArguments::default();

        for recipe in recipe_batch {
            arguments
                .add(recipe.hash().to_string())
                .map_err(|error| anyhow::anyhow!(error))?;
            arguments
                .add(serde_json::to_string(recipe)?)
                .map_err(|error| anyhow::anyhow!(error))?;
        }

        let placeholders = std::iter::repeat("(?, ?)")
            .take(recipe_batch.len())
            .join_with(", ");

        let result = sqlx::query_with(
            &format!(
                r#"
                        INSERT INTO recipes (recipe_hash, recipe_json)
                        VALUES {placeholders}
                        ON CONFLICT (recipe_hash) DO NOTHING
                    "#
            ),
            arguments,
        )
        .execute(&mut *db_transaction)
        .await?;

        num_rows_affected += result.rows_affected();
    }

    db_transaction.commit().await?;
    drop(db_conn);

    // Cache each recipe that wasn't cached before. We do this after
    // writing to the database to ensure cached items are always in the database
    let mut cached_recipes = brioche.cached_recipes.write().await;
    for recipe in uncached_recipes {
        cached_recipes.recipes_by_hash.insert(recipe.hash(), recipe);
    }

    Ok(num_rows_affected)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Meta {
    pub source: Option<Vec<StackFrame>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WithMeta<T> {
    #[serde(default, skip_serializing)]
    pub meta: Arc<Meta>,

    #[serde(flatten)]
    pub value: T,
}

impl<T> WithMeta<T> {
    pub const fn new(value: T, meta: Arc<Meta>) -> Self {
        Self { value, meta }
    }

    pub fn without_meta(value: T) -> Self {
        Self {
            value,
            meta: Arc::new(Meta::default()),
        }
    }

    pub fn as_ref(&self) -> WithMeta<&T> {
        WithMeta {
            value: &self.value,
            meta: self.meta.clone(),
        }
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> WithMeta<U> {
        WithMeta {
            value: f(self.value),
            meta: self.meta,
        }
    }

    pub fn source_frame(&self) -> Option<&StackFrame> {
        self.meta.source.as_ref().and_then(|frames| frames.first())
    }
}

impl<T, U> std::cmp::PartialEq<WithMeta<U>> for WithMeta<T>
where
    T: PartialEq<U>,
{
    fn eq(&self, other: &WithMeta<U>) -> bool {
        self.value == other.value
    }
}

impl<T> std::cmp::Eq for WithMeta<T> where T: Eq {}

impl<T> std::hash::Hash for WithMeta<T>
where
    T: std::hash::Hash,
{
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.value.hash(state);
    }
}

impl std::ops::Deref for WithMeta<Recipe> {
    type Target = Recipe;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl std::ops::DerefMut for WithMeta<Recipe> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
}

impl std::ops::Deref for WithMeta<Artifact> {
    type Target = Artifact;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl std::ops::DerefMut for WithMeta<Artifact> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackFrame {
    pub file_name: Option<String>,
    pub line_number: Option<i64>,
    pub column_number: Option<i64>,
}

impl std::fmt::Display for StackFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let file_name = self.file_name.as_deref().unwrap_or("<unknown>");
        match (self.line_number, self.column_number) {
            (Some(line), Some(column)) => {
                write!(f, "{file_name}:{line}:{column}")
            }
            (Some(line), None) => {
                write!(f, "{file_name}:{line}")
            }
            (None, _) => {
                write!(f, "{file_name}")
            }
        }
    }
}

#[serde_with::serde_as]
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDirectory {
    #[serde_as(as = "BTreeMap<TickEncoded, _>")]
    pub entries: BTreeMap<BString, WithMeta<Recipe>>,
}

impl CreateDirectory {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadRecipe {
    pub url: url::Url,
    pub hash: Hash,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Unarchive {
    pub file: Box<WithMeta<Recipe>>,
    pub archive: ArchiveFormat,
    #[serde(default)]
    pub compression: CompressionFormat,
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessRecipe {
    pub command: ProcessTemplate,

    pub args: Vec<ProcessTemplate>,

    #[serde_as(as = "BTreeMap<TickEncoded, _>")]
    pub env: BTreeMap<BString, ProcessTemplate>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<WithMeta<Recipe>>,

    pub work_dir: Box<WithMeta<Recipe>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_scaffold: Option<Box<WithMeta<Recipe>>>,

    pub platform: Platform,

    #[serde(
        rename = "unsafe",
        default,
        skip_serializing_if = "crate::utils::is_default"
    )]
    pub is_unsafe: bool,

    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub networking: bool,
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteProcessRecipe {
    pub command: CompleteProcessTemplate,

    pub args: Vec<CompleteProcessTemplate>,

    #[serde_as(as = "BTreeMap<TickEncoded, _>")]
    pub env: BTreeMap<BString, CompleteProcessTemplate>,

    #[serde_as(as = "serde_with::TryFromInto<Recipe>")]
    pub work_dir: Directory,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_scaffold: Option<Box<Artifact>>,

    pub platform: Platform,

    #[serde(
        rename = "unsafe",
        default,
        skip_serializing_if = "crate::utils::is_default"
    )]
    pub is_unsafe: bool,

    #[serde(default, skip_serializing_if = "crate::utils::is_default")]
    pub networking: bool,
}

impl TryFrom<ProcessRecipe> for CompleteProcessRecipe {
    type Error = anyhow::Error;

    fn try_from(recipe: ProcessRecipe) -> anyhow::Result<Self> {
        let ProcessRecipe {
            command,
            args,
            env,
            dependencies,
            work_dir,
            output_scaffold,
            platform,
            is_unsafe,
            networking,
        } = recipe;

        anyhow::ensure!(
            dependencies.is_empty(),
            "tried to convert process recipe to complete process recipe, but it has dependencies"
        );

        let work_dir = work_dir.value.try_into()?;
        let output_scaffold = output_scaffold
            .map(|output_scaffold| {
                let artifact: Artifact = output_scaffold.value.try_into()?;
                anyhow::Ok(Box::new(artifact))
            })
            .transpose()?;

        Ok(Self {
            command: command.try_into()?,
            args: args
                .into_iter()
                .map(TryInto::try_into)
                .collect::<Result<_, RecipeIncomplete>>()?,
            env: env
                .into_iter()
                .map(|(key, value)| Ok((key, value.try_into()?)))
                .collect::<anyhow::Result<_>>()?,
            work_dir,
            output_scaffold,
            platform,
            is_unsafe,
            networking,
        })
    }
}

#[serde_with::serde_as]
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    strum::EnumDiscriminants,
)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
#[strum_discriminants(vis(pub))]
#[strum_discriminants(derive(Hash, serde::Serialize, serde::Deserialize))]
#[strum_discriminants(serde(rename_all = "snake_case"))]
pub enum Artifact {
    #[serde(rename_all = "camelCase")]
    File(File),
    #[serde(rename_all = "camelCase")]
    Symlink {
        #[serde_as(as = "TickEncoded")]
        target: BString,
    },
    #[serde(rename_all = "camelCase")]
    Directory(Directory),
}

impl Artifact {
    pub fn try_hash(&self) -> anyhow::Result<RecipeHash> {
        let hash = RecipeHash::from_serializable(self)?;
        Ok(hash)
    }

    pub fn hash(&self) -> RecipeHash {
        self.try_hash().expect("failed to hash artifact")
    }
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct File {
    pub content_blob: BlobHash,

    pub executable: bool,

    #[serde_as(as = "serde_with::TryFromInto<Recipe>")]
    pub resources: Directory,
}

#[serde_with::serde_as]
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Directory {
    #[serde_as(as = "BTreeMap<TickEncoded, _>")]
    entries: BTreeMap<BString, RecipeHash>,
}

impl Directory {
    pub const fn from_entries(entries: BTreeMap<BString, RecipeHash>) -> Self {
        Self { entries }
    }

    pub async fn create(
        brioche: &Brioche,
        entries: &BTreeMap<BString, WithMeta<Artifact>>,
    ) -> anyhow::Result<Self> {
        let mut subdir_entries = BTreeMap::<BString, BTreeMap<_, _>>::new();
        let mut dir_entries = BTreeMap::new();

        for (path, artifact) in entries {
            match path.split_once_str("/") {
                Some((dir, subpath)) => {
                    let entries_for_subdir = subdir_entries.entry(BString::from(dir)).or_default();
                    entries_for_subdir.insert(BString::from(subpath), artifact.clone());
                }
                None => {
                    dir_entries.insert(path.clone(), artifact.clone());
                }
            }
        }

        for subdir_path in subdir_entries.keys() {
            if let Some(dir_entry) = dir_entries.remove(subdir_path) {
                let Artifact::Directory(dir) = dir_entry.value else {
                    anyhow::bail!(
                        "tried to create directory with conflicting non-directory entry at {subdir_path:?}"
                    );
                };

                anyhow::ensure!(
                    dir.is_empty(),
                    "directory at {subdir_path:?} contains conflicting values"
                );
            }
        }

        let subdir_entries = futures::stream::iter(subdir_entries)
            .then(|(dir, entries)| async move {
                let directory = Box::pin(Self::create(brioche, &entries)).await?;
                anyhow::Ok((dir, WithMeta::without_meta(Artifact::Directory(directory))))
            })
            .try_collect::<Vec<_>>()
            .await?;
        dir_entries.extend(subdir_entries);

        let recipes = dir_entries
            .values()
            .map(|recipe| Recipe::from(recipe.value.clone()));
        save_recipes(brioche, recipes).await?;

        let entries = dir_entries
            .iter()
            .map(|(path, recipe)| (path.clone(), recipe.hash()))
            .collect();
        Ok(Self { entries })
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub const fn entry_hashes(&self) -> &BTreeMap<BString, RecipeHash> {
        &self.entries
    }

    pub async fn entries(&self, brioche: &Brioche) -> anyhow::Result<BTreeMap<BString, Artifact>> {
        let entry_recipes = get_recipes(brioche, self.entries.values().copied()).await?;

        let entries = self
            .entries
            .iter()
            .map(|(path, recipe_hash)| {
                let recipe = entry_recipes
                    .get(recipe_hash)
                    .with_context(|| format!("failed to get artifact for entry {path:?}"))?;
                let artifact: Artifact = recipe.clone().try_into().map_err(|_| {
                    anyhow::anyhow!("recipe at {path:?} is not a complete artifact")
                })?;
                Ok((path.clone(), artifact))
            })
            .collect::<anyhow::Result<_>>()?;
        Ok(entries)
    }

    #[async_recursion::async_recursion]
    async fn get_by_components(
        &self,
        brioche: &Brioche,
        full_path: &BStr,
        path_components: &[&BStr],
    ) -> Result<Option<Artifact>, DirectoryError> {
        match path_components {
            [] => Err(DirectoryError::EmptyPath {
                path: full_path.into(),
            }),
            [filename] => match self.entries.get(&**filename) {
                Some(recipe_hash) => {
                    let recipe = get_recipe(brioche, *recipe_hash).await?;
                    let artifact: Artifact =
                        recipe
                            .try_into()
                            .map_err(|_| DirectoryError::RecipeIncomplete {
                                path: full_path.into(),
                            })?;
                    Ok(Some(artifact))
                }
                None => Ok(None),
            },
            [directory_name, path_components @ ..] => {
                let Some(dir_entry_hash) = self.entries.get(&**directory_name) else {
                    return Ok(None);
                };
                let dir_entry = get_recipe(brioche, *dir_entry_hash).await?;
                let dir_entry: Artifact =
                    dir_entry
                        .try_into()
                        .map_err(|_| DirectoryError::RecipeIncomplete {
                            path: full_path.into(),
                        })?;
                let Artifact::Directory(dir_entry) = dir_entry else {
                    return Err(DirectoryError::PathDescendsIntoNonDirectory {
                        path: full_path.into(),
                    });
                };
                let artifact = dir_entry
                    .get_by_components(brioche, full_path, path_components)
                    .await?;
                Ok(artifact)
            }
        }
    }

    #[async_recursion::async_recursion]
    async fn insert_by_components(
        &mut self,
        brioche: &Brioche,
        full_path: &BStr,
        path_components: &[&BStr],
        artifact: Option<Artifact>,
    ) -> Result<Option<Artifact>, DirectoryError> {
        match path_components {
            [] => {
                return Err(DirectoryError::EmptyPath {
                    path: full_path.into(),
                });
            }
            [filename] => {
                let replaced_hash = match artifact {
                    Some(artifact) => {
                        let artifact_hash = artifact.hash();
                        save_recipes(brioche, [Recipe::from(artifact)]).await?;
                        self.entries.insert(filename.to_vec().into(), artifact_hash)
                    }
                    None => self.entries.remove(&**filename),
                };
                let replaced = match replaced_hash {
                    Some(recipe_hash) => {
                        let recipe = get_recipe(brioche, recipe_hash).await?;
                        let artifact: Artifact =
                            recipe
                                .try_into()
                                .map_err(|_| DirectoryError::RecipeIncomplete {
                                    path: full_path.into(),
                                })?;
                        Some(artifact)
                    }
                    None => None,
                };
                Ok(replaced)
            }
            [directory_name, path_components @ ..] => {
                let replaced = match self.entries.entry(directory_name.to_vec().into()) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        let mut new_directory = Self::default();
                        new_directory
                            .insert_by_components(brioche, full_path, path_components, artifact)
                            .await?;

                        let new_directory: Recipe = new_directory.into();
                        let new_directory_hash = new_directory.hash();
                        save_recipes(brioche, [new_directory]).await?;

                        entry.insert(new_directory_hash);

                        None
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        let dir_entry_hash = entry.get();

                        let dir_entry = get_recipe(brioche, *dir_entry_hash).await?;
                        let dir_entry: Artifact =
                            dir_entry
                                .try_into()
                                .map_err(|_| DirectoryError::RecipeIncomplete {
                                    path: full_path.into(),
                                })?;
                        let Artifact::Directory(mut inner_dir) = dir_entry else {
                            return Err(DirectoryError::PathDescendsIntoNonDirectory {
                                path: full_path.into(),
                            });
                        };
                        let replaced = inner_dir
                            .insert_by_components(brioche, full_path, path_components, artifact)
                            .await?;

                        let updated_dir_entry: Recipe = inner_dir.into();
                        let updated_dir_entry_hash = updated_dir_entry.hash();
                        save_recipes(brioche, [updated_dir_entry]).await?;
                        entry.insert(updated_dir_entry_hash);

                        replaced
                    }
                };
                Ok(replaced)
            }
        }
    }

    pub async fn get(
        &self,
        brioche: &Brioche,
        path: &[u8],
    ) -> Result<Option<Artifact>, DirectoryError> {
        let path = bstr::BStr::new(path);
        let mut components = vec![];
        for component in path.split(|&byte| byte == b'/' || byte == b'\\') {
            if component.is_empty() || component == b"." {
                // Skip this component
            } else if component == b".." {
                // Pop the last component
                let removed_component = components.pop();
                if removed_component.is_none() {
                    return Err(DirectoryError::PathEscapes { path: path.into() });
                }
            } else {
                // Push this component
                components.push(bstr::BStr::new(component));
            }
        }

        self.get_by_components(brioche, path, &components).await
    }

    pub async fn insert(
        &mut self,
        brioche: &Brioche,
        path: &[u8],
        artifact: Option<Artifact>,
    ) -> Result<Option<Artifact>, DirectoryError> {
        let path = bstr::BStr::new(path);
        let mut components = vec![];
        for component in path.split(|&byte| byte == b'/' || byte == b'\\') {
            if component.is_empty() || component == b"." {
                // Skip this component
            } else if component == b".." {
                // Pop the last component
                let removed_component = components.pop();
                if removed_component.is_none() {
                    return Err(DirectoryError::PathEscapes { path: path.into() });
                }
            } else {
                // Push this component
                components.push(bstr::BStr::new(component));
            }
        }

        self.insert_by_components(brioche, path, &components, artifact)
            .await
    }

    #[async_recursion::async_recursion]
    pub async fn merge(&mut self, other: &Self, brioche: &Brioche) -> anyhow::Result<()> {
        for (key, artifact) in &other.entries {
            match self.entries.entry(key.clone()) {
                std::collections::btree_map::Entry::Occupied(mut current) => {
                    let (current_dir_entry, other_dir_entry) = tokio::try_join!(
                        get_recipe(brioche, *current.get()),
                        get_recipe(brioche, *artifact),
                    )?;

                    let current_dir_entry: Artifact = current_dir_entry
                        .try_into()
                        .map_err(|_| anyhow::anyhow!("current recipe at {key:?} is incomplete"))?;
                    let other_dir_entry: Artifact = other_dir_entry
                        .try_into()
                        .map_err(|_| anyhow::anyhow!("other recipe at {key:?} is incomplete"))?;
                    match (current_dir_entry, other_dir_entry) {
                        (
                            Artifact::Directory(mut current_inner),
                            Artifact::Directory(other_inner),
                        ) => {
                            current_inner.merge(&other_inner, brioche).await?;

                            let updated_current_inner_artifact: Recipe = current_inner.into();
                            let updated_current_inner_hash = updated_current_inner_artifact.hash();
                            save_recipes(brioche, [updated_current_inner_artifact]).await?;
                            current.insert(updated_current_inner_hash);
                        }
                        (_, other_dir_entry) => {
                            current.insert(other_dir_entry.hash());
                        }
                    }
                }
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(*artifact);
                }
            }
        }

        Ok(())
    }

    pub async fn glob(
        &self,
        brioche: &Brioche,
        patterns: &[wax::Glob<'_>],
    ) -> anyhow::Result<Self> {
        let mut result = Self::default();

        let entries = self.entries(brioche).await?;
        let mut queue: VecDeque<_> = entries.into_iter().collect();

        while let Some((path, artifact)) = queue.pop_front() {
            let path_string = String::from_utf8_lossy(&path);
            let any_matches = patterns
                .iter()
                .any(|pattern| pattern.is_match(&*path_string));
            if any_matches {
                result.insert(brioche, &path, Some(artifact)).await?;
            } else if let Artifact::Directory(subdirectory) = artifact {
                let sub_entries = subdirectory.entries(brioche).await?;
                queue.extend(sub_entries.into_iter().map(|(name, artifact)| {
                    let mut subpath = path.clone();
                    subpath.push_char('/');
                    subpath.extend_from_slice(&name);

                    (subpath, artifact)
                }));
            }
        }

        Ok(result)
    }
}

impl TryFrom<Recipe> for Artifact {
    type Error = RecipeIncomplete;

    fn try_from(value: Recipe) -> Result<Self, Self::Error> {
        match value {
            Recipe::File {
                content_blob: data,
                executable,
                resources,
            } => {
                let resources: Self = resources.value.try_into()?;
                let Self::Directory(resources) = resources else {
                    return Err(RecipeIncomplete);
                };
                Ok(Self::File(File {
                    content_blob: data,
                    executable,
                    resources,
                }))
            }
            Recipe::Symlink { target } => Ok(Self::Symlink { target }),
            Recipe::Directory(directory) => Ok(Self::Directory(directory)),
            Recipe::CreateDirectory(directory) if directory.is_empty() => {
                Ok(Self::Directory(Directory::default()))
            }
            Recipe::Sync { recipe } => recipe.value.try_into(),
            Recipe::Download { .. }
            | Recipe::Unarchive { .. }
            | Recipe::Process { .. }
            | Recipe::CompleteProcess { .. }
            | Recipe::CreateFile { .. }
            | Recipe::CreateDirectory(..)
            | Recipe::Cast { .. }
            | Recipe::Merge { .. }
            | Recipe::Peel { .. }
            | Recipe::Get { .. }
            | Recipe::Insert { .. }
            | Recipe::Glob { .. }
            | Recipe::SetPermissions { .. }
            | Recipe::CollectReferences { .. }
            | Recipe::AttachResources { .. }
            | Recipe::Proxy { .. } => Err(RecipeIncomplete),
        }
    }
}

impl From<Artifact> for Recipe {
    fn from(value: Artifact) -> Self {
        match value {
            Artifact::File(File {
                content_blob: data,
                executable,
                resources,
            }) => Self::File {
                content_blob: data,
                executable,
                resources: Box::new(WithMeta::without_meta(Self::Directory(resources))),
            },
            Artifact::Symlink { target } => Self::Symlink { target },
            Artifact::Directory(directory) => Self::Directory(directory),
        }
    }
}

impl From<Directory> for Recipe {
    fn from(value: Directory) -> Self {
        Self::Directory(value)
    }
}

impl TryFrom<Recipe> for Directory {
    type Error = anyhow::Error;

    fn try_from(value: Recipe) -> Result<Self, Self::Error> {
        match value {
            Recipe::Directory(directory) => Ok(directory),
            _ => {
                anyhow::bail!("expected directory recipe");
            }
        }
    }
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyRecipe {
    pub recipe: RecipeHash,
}

impl ProxyRecipe {
    pub async fn inner(&self, brioche: &Brioche) -> anyhow::Result<Recipe> {
        let inner = get_recipe(brioche, self.recipe).await?;
        Ok(inner)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("tried to convert a non-artifact recipe into an artifact")]
pub struct RecipeIncomplete;

#[derive(Debug, thiserror::Error)]
pub enum DirectoryError {
    #[error("empty path: {path:?}")]
    EmptyPath { path: bstr::BString },
    #[error("path escapes directory structure: {path:?}")]
    PathEscapes { path: bstr::BString },
    #[error("path descends into non-directory: {path:?}")]
    PathDescendsIntoNonDirectory { path: bstr::BString },
    #[error("path {path:?} contains an incomplete recipe")]
    RecipeIncomplete { path: bstr::BString },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde_with::SerializeDisplay,
    serde_with::DeserializeFromStr,
)]
pub struct RecipeHash(blake3::Hash);

impl RecipeHash {
    fn from_serializable<V>(value: &V) -> anyhow::Result<Self>
    where
        V: serde::Serialize,
    {
        let mut hasher = blake3::Hasher::new();

        json_canon::to_writer(&mut hasher, value)?;

        let hash = hasher.finalize();
        Ok(Self(hash))
    }

    pub const fn as_bytes(&self) -> &[u8; blake3::OUT_LEN] {
        self.0.as_bytes()
    }
}

impl std::fmt::Display for RecipeHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for RecipeHash {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hash = blake3::Hash::from_hex(s)?;
        Ok(Self(hash))
    }
}

impl Ord for RecipeHash {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.as_bytes().cmp(other.0.as_bytes())
    }
}

impl PartialOrd for RecipeHash {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessTemplate {
    pub components: Vec<ProcessTemplateComponent>,
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum ProcessTemplateComponent {
    Literal {
        #[serde_as(as = "TickEncoded")]
        value: BString,
    },
    Input {
        recipe: WithMeta<Recipe>,
    },
    OutputPath,
    ResourceDir,
    InputResourceDirs,
    HomeDir,
    WorkDir,
    TempDir,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteProcessTemplate {
    pub components: Vec<CompleteProcessTemplateComponent>,
}

impl CompleteProcessTemplate {
    pub fn is_empty(&self) -> bool {
        self.components.iter().all(|component| component.is_empty())
    }

    pub fn as_literal(&self) -> Option<Cow<BStr>> {
        match &*self.components {
            [CompleteProcessTemplateComponent::Literal { value }] => {
                Some(Cow::Borrowed(BStr::new(value)))
            }
            components => {
                let mut literal = vec![];
                for component in components {
                    let CompleteProcessTemplateComponent::Literal { value } = component else {
                        return None;
                    };

                    literal.extend_from_slice(value.as_bytes());
                }

                Some(Cow::Owned(BString::new(literal)))
            }
        }
    }

    pub fn split_on_literal(&self, splitter: impl AsRef<[u8]>) -> Vec<Self> {
        let mut result = vec![Self { components: vec![] }];
        for component in &self.components {
            match component {
                CompleteProcessTemplateComponent::Literal { value } => {
                    let mut splits = value.split_str(splitter.as_ref());
                    let split_first = splits.next().expect(".split_str() yielded no items");

                    if !split_first.is_empty() {
                        let current_template = result.last_mut().expect("result is empty");
                        match current_template.components.last_mut() {
                            Some(CompleteProcessTemplateComponent::Literal { value }) => {
                                value.extend_from_slice(split_first.as_bytes());
                            }
                            _ => {
                                current_template.components.push(
                                    CompleteProcessTemplateComponent::Literal {
                                        value: split_first.into(),
                                    },
                                );
                            }
                        }
                    }

                    result.extend(splits.map(|split| {
                        let components = if split.is_empty() {
                            vec![]
                        } else {
                            vec![CompleteProcessTemplateComponent::Literal {
                                value: split.into(),
                            }]
                        };

                        Self { components }
                    }));
                }
                component => {
                    let current_template = result.last_mut().expect("result is empty");
                    current_template.components.push(component.clone());
                }
            }
        }

        result
    }

    pub fn append_literal(&mut self, literal: impl AsRef<[u8]>) {
        if let Some(CompleteProcessTemplateComponent::Literal { value }) =
            self.components.last_mut()
        {
            value.extend_from_slice(literal.as_ref());
        } else {
            self.components
                .push(CompleteProcessTemplateComponent::Literal {
                    value: literal.as_ref().into(),
                });
        }
    }
}

impl TryFrom<ProcessTemplate> for CompleteProcessTemplate {
    type Error = RecipeIncomplete;

    fn try_from(value: ProcessTemplate) -> Result<Self, RecipeIncomplete> {
        let components = value
            .components
            .into_iter()
            .map(TryInto::try_into)
            .collect::<Result<_, RecipeIncomplete>>()?;
        Ok(Self { components })
    }
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum CompleteProcessTemplateComponent {
    Literal {
        #[serde_as(as = "TickEncoded")]
        value: BString,
    },
    Input {
        artifact: WithMeta<Artifact>,
    },
    OutputPath,
    ResourceDir,
    InputResourceDirs,
    HomeDir,
    WorkDir,
    TempDir,
}

impl CompleteProcessTemplateComponent {
    fn is_empty(&self) -> bool {
        match self {
            Self::Literal { value } => value.is_empty(),
            _ => false,
        }
    }
}

impl TryFrom<ProcessTemplateComponent> for CompleteProcessTemplateComponent {
    type Error = RecipeIncomplete;

    fn try_from(value: ProcessTemplateComponent) -> Result<Self, Self::Error> {
        match value {
            ProcessTemplateComponent::Literal { value } => Ok(Self::Literal { value }),
            ProcessTemplateComponent::Input { recipe } => {
                let artifact = recipe.value.try_into()?;
                let artifact = WithMeta::new(artifact, recipe.meta);
                Ok(Self::Input { artifact })
            }
            ProcessTemplateComponent::OutputPath => Ok(Self::OutputPath),
            ProcessTemplateComponent::ResourceDir => Ok(Self::ResourceDir),
            ProcessTemplateComponent::InputResourceDirs => Ok(Self::InputResourceDirs),
            ProcessTemplateComponent::HomeDir => Ok(Self::HomeDir),
            ProcessTemplateComponent::WorkDir => Ok(Self::WorkDir),
            ProcessTemplateComponent::TempDir => Ok(Self::TempDir),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveFormat {
    Tar,
    Zip,
}

#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum CompressionFormat {
    #[default]
    None,
    Bzip2,
    Gzip,
    Xz,
    Zstd,
}

impl CompressionFormat {
    pub fn decompress(
        &self,
        input: impl tokio::io::AsyncBufRead + Unpin + Send + 'static,
    ) -> Box<dyn tokio::io::AsyncRead + Unpin + Send> {
        match self {
            Self::None => Box::new(input),
            Self::Bzip2 => Box::new(async_compression::tokio::bufread::BzDecoder::new(input)),
            Self::Gzip => Box::new(async_compression::tokio::bufread::GzipDecoder::new(input)),
            Self::Xz => Box::new(async_compression::tokio::bufread::XzDecoder::new(input)),
            Self::Zstd => Box::new(async_compression::tokio::bufread::ZstdDecoder::new(input)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CompleteProcessTemplate, CompleteProcessTemplateComponent};

    fn tpl(
        components: impl IntoIterator<Item = CompleteProcessTemplateComponent>,
    ) -> CompleteProcessTemplate {
        CompleteProcessTemplate {
            components: components.into_iter().collect(),
        }
    }

    fn literal(value: impl AsRef<[u8]>) -> CompleteProcessTemplateComponent {
        CompleteProcessTemplateComponent::Literal {
            value: value.as_ref().into(),
        }
    }

    const fn output_path() -> CompleteProcessTemplateComponent {
        CompleteProcessTemplateComponent::OutputPath
    }

    const fn work_dir() -> CompleteProcessTemplateComponent {
        CompleteProcessTemplateComponent::WorkDir
    }

    #[test]
    fn test_complete_process_template_split() {
        assert_eq!(tpl([]).split_on_literal(":"), vec![tpl([])]);

        assert_eq!(
            tpl([literal("hello world")]).split_on_literal(":"),
            vec![tpl([literal("hello world")])],
        );

        assert_eq!(
            tpl([literal("hello:world")]).split_on_literal(":"),
            vec![tpl([literal("hello")]), tpl([literal("world")])],
        );

        assert_eq!(
            tpl([literal("a:b::d")]).split_on_literal(":"),
            vec![
                tpl([literal("a")]),
                tpl([literal("b")]),
                tpl([]),
                tpl([literal("d")])
            ],
        );

        assert_eq!(
            tpl([literal("asdf")]).split_on_literal(""),
            vec![
                tpl([]),
                tpl([literal("a")]),
                tpl([literal("s")]),
                tpl([literal("d")]),
                tpl([literal("f")]),
                tpl([]),
            ],
        );

        assert_eq!(
            tpl([
                output_path(),
                literal("/foo:"),
                output_path(),
                literal("/bar")
            ])
            .split_on_literal(":"),
            vec![
                tpl([output_path(), literal("/foo")]),
                tpl([output_path(), literal("/bar")])
            ]
        );

        assert_eq!(
            tpl([
                output_path(),
                literal("/foo:/asdf:"),
                output_path(),
                literal("/bar")
            ])
            .split_on_literal(":"),
            vec![
                tpl([output_path(), literal("/foo")]),
                tpl([literal("/asdf")]),
                tpl([output_path(), literal("/bar")]),
            ]
        );

        assert_eq!(
            tpl([
                output_path(),
                work_dir(),
                literal("/foo:/asdf:"),
                work_dir(),
                output_path(),
                literal("/bar")
            ])
            .split_on_literal(":"),
            vec![
                tpl([output_path(), work_dir(), literal("/foo")]),
                tpl([literal("/asdf")]),
                tpl([work_dir(), output_path(), literal("/bar")]),
            ]
        );

        assert_eq!(
            tpl([
                output_path(),
                work_dir(),
                literal("/foo::/asdf:"),
                work_dir(),
                output_path(),
                literal("/bar")
            ])
            .split_on_literal(":"),
            vec![
                tpl([output_path(), work_dir(), literal("/foo")]),
                tpl([]),
                tpl([literal("/asdf")]),
                tpl([work_dir(), output_path(), literal("/bar")]),
            ]
        );

        assert_eq!(
            tpl([
                output_path(),
                work_dir(),
                literal("foo"),
                output_path(),
                work_dir(),
                literal("bar")
            ])
            .split_on_literal(""),
            vec![
                tpl([output_path(), work_dir()]),
                tpl([literal("f")]),
                tpl([literal("o")]),
                tpl([literal("o")]),
                tpl([output_path(), work_dir()]),
                tpl([literal("b")]),
                tpl([literal("a")]),
                tpl([literal("r")]),
                tpl([]),
            ]
        );
    }
}
