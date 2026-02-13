//! Types and utilities for handling paths across various platforms.
//!
//! There's a hierarchy of different path types, starting with [`AnyPath`]
//! as the most general:
//!
//! - [`AnyPath`]
//!     - [`RelativePath`]
//!     - [`AbsolutePath`]
//!     - [`BasePath`] (distinct from [`RootPath`], but mostly exists
//!       for Windows)
//!         - [`RootPath`]

use bstr::ByteSlice as _;
use joinery::JoinableIterator as _;

/// Any filesystem path for any (supported) platform.
///
/// Generally, this can be either a [`RelativePath`] or an [`AbsolutePath`].
/// However, there are some types of Windows paths that are considered
/// neither relative nor absolute.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnyPath {
    base: Option<BasePath>,
    subpath: RelativePath,
}

impl AnyPath {
    pub fn to_system_path(&self) -> Result<std::path::PathBuf, ToSystemPathError> {
        to_system_path(self.base.as_ref(), &self.subpath)
    }
}

impl std::fmt::Display for AnyPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { base, subpath } = self;
        if let Some(base) = base {
            write!(f, "{base}")?;
        }
        write!(f, "{subpath}")
    }
}

impl std::fmt::Debug for AnyPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AnyPath({self})")
    }
}

impl From<RelativePath> for AnyPath {
    fn from(path: RelativePath) -> Self {
        Self {
            base: None,
            subpath: path,
        }
    }
}

impl From<AbsolutePath> for AnyPath {
    fn from(path: AbsolutePath) -> Self {
        let AbsolutePath { root, subpath } = path;
        Self {
            base: Some(root.into()),
            subpath,
        }
    }
}

impl From<BasePath> for AnyPath {
    fn from(path: BasePath) -> Self {
        Self {
            base: Some(path),
            subpath: RelativePath::default(),
        }
    }
}

impl From<RootPath> for AnyPath {
    fn from(path: RootPath) -> Self {
        Self {
            base: Some(path.into()),
            subpath: RelativePath::default(),
        }
    }
}

/// A relative filesystem path. If a path isn't an [`AbsolutePath`], it's
/// (probably) a [`RelativePath`]. For example, `foo/bar.txt`, `file.txt`,
/// `../other`, and `.` are all valid relative paths.
///
/// A relative path consists of several components, each of which can be
/// a normal filename, or the special "current dir" (`.`) or "parent dir"
/// (`..`) components.
///
/// A [`RelativePath`] can contain characters that aren't valid for all
/// platforms, including illegal characters or path separators. This means
/// that converting a [`RelativePath`] to a [`std::path::Path`] or otherwise
/// using a relative path is always fallible.
///
/// `/` is often used as a path separator when displaying or parsing a
/// relative path by convention, but [`RelativePath`] itself is agnostic
/// to the path separator.
#[derive(Default, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelativePath {
    components: Vec<RelativePathComponent>,
}

impl RelativePath {
    #[must_use]
    pub fn one(component: impl Into<RelativePathComponent>) -> Self {
        Self {
            components: vec![component.into()],
        }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.components.is_empty()
    }

    fn add_one(&mut self, component: impl Into<RelativePathComponent>) {
        self.components.push(component.into());
    }

    #[must_use]
    pub fn join_one(&self, component: impl Into<RelativePathComponent>) -> Self {
        let mut new = self.clone();
        new.add_one(component.into());
        new
    }

    fn add(&mut self, other: Self) {
        self.components.extend(other.components);
    }

    #[must_use]
    pub fn join(&self, other: Self) -> Self {
        let mut new = self.clone();
        new.add(other);
        new
    }

    fn add_subpath(&mut self, mut subpath: Self) -> Result<(), SubpathError> {
        subpath.normalize_subpath()?;
        self.add(subpath);
        Ok(())
    }

    pub fn join_subpath(&self, subpath: Self) -> Result<Self, SubpathError> {
        let mut new = self.clone();
        new.add_subpath(subpath)?;
        Ok(new)
    }

    fn normalize_logical(&mut self) {
        let (logical_components, ascends) =
            logical_components(std::mem::take(&mut self.components));
        self.components
            .extend((0..ascends).map(|_| RelativePathComponent::ParentDir));
        self.components.extend(
            logical_components
                .into_iter()
                .map(RelativePathComponent::Normal),
        );
    }

    #[must_use]
    pub fn normalized_logical(&self) -> Self {
        let mut subpath = self.clone();
        subpath.normalize_logical();
        subpath
    }

    fn normalize_subpath(&mut self) -> Result<(), SubpathError> {
        let subpath_components = subpath_components(std::mem::take(&mut self.components))?;
        self.components = subpath_components
            .into_iter()
            .map(RelativePathComponent::Normal)
            .collect();
        Ok(())
    }

    pub fn normalized_subpath(&self) -> Result<Self, SubpathError> {
        let mut subpath = self.clone();
        subpath.normalize_subpath()?;
        Ok(subpath)
    }

    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        let mut parent = self.clone();
        let popped = parent.components.pop();
        Some(parent).filter(|_| popped.is_some())
    }

    #[must_use]
    pub fn filename(&self) -> Option<&bstr::BStr> {
        self.components
            .last()
            .and_then(|component| match component {
                RelativePathComponent::CurrentDir | RelativePathComponent::ParentDir => None,
                RelativePathComponent::Normal(filename) => Some(bstr::BStr::new(filename)),
            })
    }

    pub fn to_system_path(&self) -> Result<std::path::PathBuf, ToSystemPathError> {
        to_system_path(None, self)
    }
}

impl From<&str> for RelativePath {
    fn from(value: &str) -> Self {
        Self {
            components: value.split('/').map(RelativePathComponent::from).collect(),
        }
    }
}

impl From<&[u8]> for RelativePath {
    fn from(value: &[u8]) -> Self {
        Self {
            components: value
                .split(|b| *b == b'/')
                .map(RelativePathComponent::from)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RelativePathComponent {
    CurrentDir,
    ParentDir,
    Normal(bstr::BString),
}

impl std::fmt::Display for RelativePathComponent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CurrentDir => write!(f, "."),
            Self::ParentDir => write!(f, ".."),
            Self::Normal(normal) => write!(f, "{normal}"),
        }
    }
}

impl From<&str> for RelativePathComponent {
    fn from(value: &str) -> Self {
        match value {
            "." => Self::CurrentDir,
            ".." => Self::ParentDir,
            value => Self::Normal(value.into()),
        }
    }
}

impl From<String> for RelativePathComponent {
    fn from(value: String) -> Self {
        match &*value {
            "." => Self::CurrentDir,
            ".." => Self::ParentDir,
            _ => Self::Normal(value.into()),
        }
    }
}

impl From<&[u8]> for RelativePathComponent {
    fn from(value: &[u8]) -> Self {
        match value {
            b"." => Self::CurrentDir,
            b".." => Self::ParentDir,
            value => Self::Normal(value.into()),
        }
    }
}

impl From<Vec<u8>> for RelativePathComponent {
    fn from(value: Vec<u8>) -> Self {
        match &*value {
            b"." => Self::CurrentDir,
            b".." => Self::ParentDir,
            _ => Self::Normal(value.into()),
        }
    }
}

impl std::fmt::Display for RelativePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // TODO: Allow customizing the separator with validation, infer
        // an appropriate default separator
        write!(f, "{}", self.components.iter().join_with('/'))
    }
}

impl std::fmt::Debug for RelativePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RelativePath({self})")
    }
}

/// An absolute path for any (supported) platform.
///
/// Conceptually, an absolute path is what you get if you normalize an
/// [`AnyPath`]. An absolute path consists of a [`RootPath`] plus a
/// [`RelativePath`] subpath.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AbsolutePath {
    root: RootPath,
    subpath: RelativePath,
}

impl AbsolutePath {
    pub fn add_one(&mut self, component: impl Into<RelativePathComponent>) {
        self.subpath.add_one(component.into());
    }

    #[must_use]
    pub fn join_one(&self, component: impl Into<RelativePathComponent>) -> Self {
        let mut new = self.clone();
        new.add_one(component.into());
        new
    }

    pub fn join_subpath(&self, subpath: RelativePath) -> Result<Self, SubpathError> {
        let new_subpath = self.subpath.join_subpath(subpath)?;
        Ok(Self {
            root: self.root.clone(),
            subpath: new_subpath,
        })
    }

    pub fn to_system_path(&self) -> Result<std::path::PathBuf, ToSystemPathError> {
        to_system_path(Some(&BasePath::Root(self.root.clone())), &self.subpath)
    }
}

impl std::fmt::Display for AbsolutePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { root, subpath } = self;
        write!(f, "{root}{subpath}")
    }
}

impl std::fmt::Debug for AbsolutePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AbsolutePath({self})")
    }
}

/// The base component of a path.
///
/// This is a superset of [`RootPath`]s to support paths that don't fit
/// within a [`RelativePath`] nor an [`AbsolutePath`].
///
/// This is mainly used to represent certain types of Windows paths. On
/// Windows, a normal (DOS-style) absolute path is written like this:
///
/// ```plain
/// C:\Windows\system32\notepad.exe
/// ```
///
/// ...but these are also valid DOS-style filesystem paths on Windows:
///
/// - `C:system32\notepad.exe` - A path relative to the current dir of the
///   `C:` drive.
/// - `\Windows\system32\notepad.exe` - A path relative to the root of the
///   current drive.
///
/// This page from the Microsoft docs explains things in more depth:
///
/// <https://learn.microsoft.com/en-us/dotnet/standard/io/file-path-formats>
///
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum BasePath {
    Root(RootPath),
    WindowsCurrentDriveRoot,
    WindowsDriveRelative { drive_letter: u8 },
}

impl std::fmt::Display for BasePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Root(root) => write!(f, "{root}"),
            Self::WindowsCurrentDriveRoot => write!(f, r"\"),
            Self::WindowsDriveRelative { drive_letter } => {
                write!(f, r"{}:", char::from(*drive_letter))
            }
        }
    }
}

impl std::fmt::Debug for BasePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BasePath({self})")
    }
}

impl From<RootPath> for BasePath {
    fn from(path: RootPath) -> Self {
        Self::Root(path)
    }
}

/// The start of of an [`AbsolutePath`]. Different platforms are distinct
/// in the type(s) of root path used.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RootPath {
    /// The root directory for a Unix-style filesystem path: `/`
    UnixRoot,

    /// The root of a drive for a (DOS-style) Windows filesystem path: `C:\`
    WindowsDriveRoot { drive_letter: u8 },

    /// The root of a Windows UNC path, such as a network share: `\\Server\`
    WindowsUnc { host: bstr::BString },
}

impl RootPath {
    pub fn to_system_path(&self) -> Result<std::path::PathBuf, ToSystemPathError> {
        to_system_path(
            Some(&BasePath::Root(self.clone())),
            &RelativePath::default(),
        )
    }
}

impl std::fmt::Display for RootPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnixRoot => write!(f, "/"),
            Self::WindowsDriveRoot { drive_letter } => {
                write!(f, r"{}:\", char::from(*drive_letter))
            }
            Self::WindowsUnc { host } => write!(f, r"\\{host}\"),
        }
    }
}

pub fn from_system_path(path: &std::path::Path) -> Result<AnyPath, FromSystemPathError> {
    let mut base: Option<BasePath> = None;
    let mut subpath = RelativePath::default();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => match prefix.kind() {
                std::path::Prefix::Verbatim(component) => {
                    let component = <[u8]>::from_os_str(component)
                        .ok_or(FromSystemPathError::Unrepresentable)?;

                    base = Some(BasePath::Root(RootPath::WindowsUnc { host: "?".into() }));
                    subpath.add_one(component);
                }
                std::path::Prefix::VerbatimUNC(hostname, share) => {
                    let hostname = <[u8]>::from_os_str(hostname)
                        .ok_or(FromSystemPathError::Unrepresentable)?;
                    let share =
                        <[u8]>::from_os_str(share).ok_or(FromSystemPathError::Unrepresentable)?;

                    base = Some(BasePath::Root(RootPath::WindowsUnc { host: "?".into() }));
                    subpath.add_one("UNC");
                    subpath.add_one(hostname);
                    subpath.add_one(share);
                }
                std::path::Prefix::VerbatimDisk(drive_letter) => {
                    base = Some(BasePath::Root(RootPath::WindowsUnc { host: "?".into() }));
                    subpath.add_one(format!("{}:", char::from(drive_letter)));
                }
                std::path::Prefix::DeviceNS(component) => {
                    let component = <[u8]>::from_os_str(component)
                        .ok_or(FromSystemPathError::Unrepresentable)?;

                    base = Some(BasePath::Root(RootPath::WindowsUnc { host: ".".into() }));
                    subpath.add_one(component);
                }
                std::path::Prefix::UNC(hostname, share) => {
                    let hostname = <[u8]>::from_os_str(hostname)
                        .ok_or(FromSystemPathError::Unrepresentable)?;
                    let share =
                        <[u8]>::from_os_str(share).ok_or(FromSystemPathError::Unrepresentable)?;

                    base = Some(BasePath::Root(RootPath::WindowsUnc {
                        host: hostname.into(),
                    }));
                    subpath.add_one(share);
                }
                std::path::Prefix::Disk(drive_letter) => {
                    base = Some(BasePath::WindowsDriveRelative { drive_letter });
                }
            },
            std::path::Component::RootDir => {
                base = match base {
                    base @ Some(BasePath::Root(_) | BasePath::WindowsCurrentDriveRoot) => base,
                    Some(BasePath::WindowsDriveRelative { drive_letter }) => {
                        Some(BasePath::Root(RootPath::WindowsDriveRoot { drive_letter }))
                    }
                    None => {
                        if cfg!(target_family = "windows") {
                            Some(BasePath::WindowsCurrentDriveRoot)
                        } else if cfg!(any(target_family = "unix", target_family = "wasm")) {
                            Some(BasePath::Root(RootPath::UnixRoot))
                        } else {
                            unimplemented!("encountered root dir component on unhandled platform");
                        }
                    }
                };
            }
            std::path::Component::CurDir => {
                subpath.components.push(RelativePathComponent::CurrentDir);
            }
            std::path::Component::ParentDir => {
                subpath.components.push(RelativePathComponent::ParentDir);
            }
            std::path::Component::Normal(component) => {
                let component =
                    <[u8]>::from_os_str(component).ok_or(FromSystemPathError::Unrepresentable)?;
                subpath
                    .components
                    .push(RelativePathComponent::Normal(component.into()));
            }
        }
    }

    Ok(AnyPath { base, subpath })
}

pub async fn canonicalize_system_path(
    path: &std::path::Path,
) -> Result<AbsolutePath, CanonicalizeSystemPathError> {
    let path = tokio::fs::canonicalize(path).await?;
    let path = from_system_path(&path)?;

    match path.base {
        Some(BasePath::Root(root)) => Ok(AbsolutePath {
            root,
            subpath: path.subpath,
        }),
        _ => Err(CanonicalizeSystemPathError::NotAnAbsolutePath),
    }
}

fn to_system_path(
    base_path: Option<&BasePath>,
    subpath: &RelativePath,
) -> Result<std::path::PathBuf, ToSystemPathError> {
    let mut path: std::path::PathBuf = match base_path {
        Some(BasePath::Root(RootPath::UnixRoot)) => {
            if cfg!(any(target_family = "unix", target_family = "wasm")) {
                "/".into()
            } else {
                return Err(ToSystemPathError::InvalidBasePathForPlatform);
            }
        }
        Some(BasePath::Root(RootPath::WindowsDriveRoot { drive_letter })) => {
            if cfg!(target_family = "windows") {
                format!(r"{}:\", char::from(*drive_letter)).into()
            } else {
                return Err(ToSystemPathError::InvalidBasePathForPlatform);
            }
        }
        Some(BasePath::Root(RootPath::WindowsUnc { host })) => {
            if cfg!(target_family = "windows") {
                format!(r"\\{host}\").into()
            } else {
                return Err(ToSystemPathError::InvalidBasePathForPlatform);
            }
        }
        Some(BasePath::WindowsCurrentDriveRoot) => {
            if cfg!(target_family = "windows") {
                r"\".into()
            } else {
                return Err(ToSystemPathError::InvalidBasePathForPlatform);
            }
        }
        Some(BasePath::WindowsDriveRelative { drive_letter }) => {
            if cfg!(target_family = "windows") {
                format!(r"{}:", char::from(*drive_letter)).into()
            } else {
                return Err(ToSystemPathError::InvalidBasePathForPlatform);
            }
        }
        None => std::path::PathBuf::new(),
    };

    for component in &subpath.components {
        match component {
            RelativePathComponent::CurrentDir => path.push("."),
            RelativePathComponent::ParentDir => path.push(".."),
            RelativePathComponent::Normal(filename) => {
                if cfg!(target_family = "windows") {
                    if filename.find_byteset(b"<>:\"/\\|?*\0").is_some() {
                        return Err(ToSystemPathError::InvalidFilenameForPlatform(
                            filename.clone(),
                        ));
                    }
                } else if cfg!(any(target_family = "unix", target_family = "wasm")) {
                    if filename.find_byteset(b"/\0").is_some() {
                        return Err(ToSystemPathError::InvalidFilenameForPlatform(
                            filename.clone(),
                        ));
                    }
                } else {
                    unimplemented!("no path validation configured for the current platform");
                }

                let filename = filename.to_os_str().map_err(ToSystemPathError::Utf8Error)?;
                path.push(filename);
            }
        }
    }

    Ok(path)
}

fn subpath_components(
    components: Vec<RelativePathComponent>,
) -> Result<Vec<bstr::BString>, SubpathError> {
    let (new_components, ascends) = logical_components(components);
    if ascends == 0 {
        Ok(new_components)
    } else {
        Err(SubpathError::SubpathEscapesTopLevel)
    }
}

fn logical_components(components: Vec<RelativePathComponent>) -> (Vec<bstr::BString>, usize) {
    let mut ascends = 0;
    let mut new_components = vec![];
    for component in components {
        match component {
            RelativePathComponent::CurrentDir => {}
            RelativePathComponent::ParentDir => {
                let popped = new_components.pop();
                if popped.is_none() {
                    ascends += 1;
                }
            }
            RelativePathComponent::Normal(normal) => {
                new_components.push(normal);
            }
        }
    }

    (new_components, ascends)
}

#[derive(Debug, thiserror::Error)]
pub enum FromSystemPathError {
    #[error("unrepresentable path")]
    Unrepresentable,
}

#[derive(Debug, thiserror::Error)]
pub enum ToSystemPathError {
    #[error("base path is not valid for the current platform")]
    InvalidBasePathForPlatform,

    #[error("filename '{0}' within path is not valid for the current platform")]
    InvalidFilenameForPlatform(bstr::BString),

    #[error(transparent)]
    Utf8Error(bstr::Utf8Error),
}

#[derive(Debug, thiserror::Error)]
pub enum CanonicalizeSystemPathError {
    #[error(transparent)]
    FromSystemPath(#[from] FromSystemPathError),

    #[error(transparent)]
    IoError(#[from] std::io::Error),

    #[error("not an absolute path")]
    NotAnAbsolutePath,
}

#[derive(Debug, thiserror::Error)]
pub enum SubpathError {
    #[error("subpath escapes top-level path")]
    SubpathEscapesTopLevel,
}
