use std::{
    collections::{HashSet, VecDeque},
    ffi::OsStr,
    path::{Path, PathBuf},
};

use bstr::ByteSlice as _;

#[derive(Debug, Clone, Copy)]
pub struct AutowrapOptions<'a> {
    pub program_path: &'a Path,
    pub packed_exec_path: &'a Path,
    pub resource_dir: &'a Path,
    pub all_resource_dirs: &'a [PathBuf],
    pub sysroot: &'a Path,
    pub library_search_paths: &'a [PathBuf],
    pub input_paths: &'a [PathBuf],
    pub skip_libs: &'a [String],
    pub skip_unknown_libs: bool,
    pub runtime_library_dirs: &'a [PathBuf],
}

pub fn autowrap(options: AutowrapOptions) -> Result<(), AutowrapError> {
    let program_file = std::fs::read(options.program_path)?;
    let program_object = goblin::Object::parse(&program_file)?;

    match program_object {
        goblin::Object::Elf(elf) => {
            if let Some(program_interpreter) = elf.interpreter {
                let program_interpreter_path = PathBuf::from(program_interpreter);
                let relative_interpreter_path =
                    program_interpreter_path.strip_prefix("/").map_err(|_| {
                        AutowrapError::UnsupportedInterpreterPath(program_interpreter.to_owned())
                    })?;
                let interpreter_path = options.sysroot.join(relative_interpreter_path);
                if !interpreter_path.is_file() {
                    return Err(AutowrapError::UnsupportedInterpreterPath(
                        program_interpreter.to_owned(),
                    ));
                }

                let pack = dynamic_ld_linux_elf_pack(DynamicLdLinuxElfPackOptions {
                    program_path: options.program_path,
                    program_contents: &program_file,
                    resource_dir: options.resource_dir,
                    all_resource_dirs: options.all_resource_dirs,
                    interpreter_path: &interpreter_path,
                    library_search_paths: options.library_search_paths,
                    input_paths: options.input_paths,
                    skip_libs: options.skip_libs,
                    skip_unknown_libs: options.skip_unknown_libs,
                    elf: &elf,
                    runtime_library_dirs: options.runtime_library_dirs,
                })?;

                let mut packed = std::fs::File::open(options.packed_exec_path)?;
                let mut packed_program = std::fs::File::create(options.program_path)?;
                std::io::copy(&mut packed, &mut packed_program)?;

                crate::inject_pack(&mut packed_program, &pack)?;
            } else {
                let pack = static_elf_pack(StaticElfPackOptions {
                    resource_dir: options.resource_dir,
                    all_resource_dirs: options.all_resource_dirs,
                    library_search_paths: options.library_search_paths,
                    input_paths: options.input_paths,
                    skip_libs: options.skip_libs,
                    skip_unknown_libs: options.skip_unknown_libs,
                    elf: &elf,
                })?;

                if pack.should_add_to_executable() {
                    let mut program = std::fs::OpenOptions::new()
                        .append(true)
                        .open(options.program_path)?;
                    crate::inject_pack(&mut program, &pack)?;
                }
            }
        }
        goblin::Object::Archive(_) => {
            // Nothing to do
        }
        _ => {
            unimplemented!("unsupported output type");
        }
    }

    Ok(())
}

struct DynamicLdLinuxElfPackOptions<'a> {
    program_path: &'a Path,
    program_contents: &'a [u8],
    resource_dir: &'a Path,
    all_resource_dirs: &'a [PathBuf],
    interpreter_path: &'a Path,
    library_search_paths: &'a [PathBuf],
    input_paths: &'a [PathBuf],
    skip_libs: &'a [String],
    skip_unknown_libs: bool,
    elf: &'a goblin::elf::Elf<'a>,
    runtime_library_dirs: &'a [PathBuf],
}

fn dynamic_ld_linux_elf_pack(
    options: DynamicLdLinuxElfPackOptions,
) -> Result<crate::Pack, AutowrapError> {
    let program_name = options
        .program_path
        .file_name()
        .ok_or_else(|| AutowrapError::InvalidPath)?;
    let resource_program_path = crate::resources::add_named_blob(
        options.resource_dir,
        std::io::Cursor::new(&options.program_contents),
        is_path_executable(options.program_path)?,
        Path::new(program_name),
    )?;

    let interpreter_name = options
        .interpreter_path
        .file_name()
        .ok_or_else(|| AutowrapError::InvalidPath)?;
    let interpreter = std::fs::File::open(options.interpreter_path)?;
    let resource_interpreter_path = crate::resources::add_named_blob(
        options.resource_dir,
        interpreter,
        is_path_executable(options.interpreter_path)?,
        Path::new(interpreter_name),
    )?;
    let resource_interpreter_path = <[u8]>::from_path(&resource_interpreter_path)
        .ok_or_else(|| AutowrapError::InvalidPath)?
        .into();

    let find_library_options = FindLibraryOptions {
        resource_dir: options.resource_dir,
        all_resource_dirs: options.all_resource_dirs,
        library_search_paths: options.library_search_paths,
        input_paths: options.input_paths,
        skip_libs: options.skip_libs,
        skip_unknown_libs: options.skip_unknown_libs,
    };

    let resource_library_dirs = collect_all_library_dirs(&find_library_options, options.elf)?;

    let resource_program_path = <[u8]>::from_path(&resource_program_path)
        .ok_or_else(|| AutowrapError::InvalidPath)?
        .into();
    let runtime_library_dirs = options
        .runtime_library_dirs
        .iter()
        .map(|dir| {
            let dir = <[u8]>::from_path(dir).ok_or(AutowrapError::InvalidPath)?;
            Ok::<_, AutowrapError>(dir.to_vec())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let pack = crate::Pack::LdLinux {
        program: resource_program_path,
        interpreter: resource_interpreter_path,
        library_dirs: resource_library_dirs,
        runtime_library_dirs,
    };

    Ok(pack)
}

struct StaticElfPackOptions<'a> {
    resource_dir: &'a Path,
    all_resource_dirs: &'a [PathBuf],
    library_search_paths: &'a [PathBuf],
    input_paths: &'a [PathBuf],
    skip_libs: &'a [String],
    skip_unknown_libs: bool,
    elf: &'a goblin::elf::Elf<'a>,
}

fn static_elf_pack(options: StaticElfPackOptions) -> Result<crate::Pack, AutowrapError> {
    let find_library_options = FindLibraryOptions {
        resource_dir: options.resource_dir,
        all_resource_dirs: options.all_resource_dirs,
        library_search_paths: options.library_search_paths,
        input_paths: options.input_paths,
        skip_libs: options.skip_libs,
        skip_unknown_libs: options.skip_unknown_libs,
    };

    let resource_library_dirs = collect_all_library_dirs(&find_library_options, options.elf)?;

    let pack = crate::Pack::Static {
        library_dirs: resource_library_dirs,
    };

    Ok(pack)
}

struct FindLibraryOptions<'a> {
    resource_dir: &'a Path,
    all_resource_dirs: &'a [PathBuf],
    library_search_paths: &'a [PathBuf],
    input_paths: &'a [PathBuf],
    skip_libs: &'a [String],
    skip_unknown_libs: bool,
}

fn collect_all_library_dirs(
    options: &FindLibraryOptions,
    elf: &goblin::elf::Elf,
) -> Result<Vec<Vec<u8>>, AutowrapError> {
    let mut all_search_paths = options.library_search_paths.to_vec();

    let mut resource_library_dirs = vec![];
    let mut needed_libraries = elf
        .libraries
        .iter()
        .map(|lib| lib.to_string())
        .collect::<VecDeque<_>>();
    let mut found_libraries = HashSet::new();
    let skip_libraries = options.skip_libs.iter().collect::<HashSet<_>>();

    while let Some(original_library_name) = needed_libraries.pop_front() {
        if found_libraries.contains(&original_library_name) {
            continue;
        }

        if skip_libraries.contains(&original_library_name) {
            continue;
        }

        let library_path_result = find_library(
            &FindLibraryOptions {
                input_paths: options.input_paths,
                library_search_paths: &all_search_paths,
                resource_dir: options.resource_dir,
                all_resource_dirs: options.all_resource_dirs,
                skip_libs: options.skip_libs,
                skip_unknown_libs: options.skip_unknown_libs,
            },
            &original_library_name,
        );
        let library_path = match library_path_result {
            Ok(library_path) => library_path,
            Err(AutowrapError::LibraryNotFound(_)) if options.skip_unknown_libs => {
                continue;
            }
            Err(err) => {
                return Err(err);
            }
        };
        let library_name = std::path::PathBuf::from(&original_library_name);
        let library_name = library_name
            .file_name()
            .ok_or_else(|| AutowrapError::InvalidPath)?;

        found_libraries.insert(original_library_name);

        let library = std::fs::File::open(&library_path)?;
        let resource_library_path = crate::resources::add_named_blob(
            options.resource_dir,
            library,
            is_path_executable(&library_path)?,
            Path::new(library_name),
        )?;
        let resource_library_dir = resource_library_path
            .parent()
            .expect("no parent dir for library path");
        let resource_library_dir = <[u8]>::from_path(resource_library_dir)
            .ok_or_else(|| AutowrapError::InvalidPath)?
            .into();

        resource_library_dirs.push(resource_library_dir);

        // Try to get dynamic dependencies from the library itself

        let Ok(library_file) = std::fs::read(&library_path) else {
            continue;
        };
        let Ok(library_object) = goblin::Object::parse(&library_file) else {
            continue;
        };

        // TODO: Support other object files
        let library_elf = match library_object {
            goblin::Object::Elf(elf) => elf,
            _ => continue,
        };
        needed_libraries.extend(library_elf.libraries.iter().map(|lib| lib.to_string()));

        if let Ok(library_pack) = crate::extract_pack(&library_file[..]) {
            let library_dirs = match &library_pack {
                crate::Pack::LdLinux { library_dirs, .. } => library_dirs,
                crate::Pack::Static { library_dirs } => library_dirs,
            };

            for library_dir in library_dirs {
                let Ok(library_dir) = library_dir.to_path() else {
                    continue;
                };
                let Some(library_dir_path) =
                    crate::find_in_resource_dirs(options.all_resource_dirs, library_dir)
                else {
                    continue;
                };

                all_search_paths.push(library_dir_path);
            }
        }
    }

    Ok(resource_library_dirs)
}

fn find_library(
    options: &FindLibraryOptions,
    library_name: &str,
) -> Result<std::path::PathBuf, AutowrapError> {
    // Search for the library from the search paths passed to `ld`, searching
    // for a filename match.
    for lib_path in options.library_search_paths {
        let lib_path = lib_path.join(library_name);

        if lib_path.is_file() {
            return Ok(lib_path);
        }
    }

    // Search for the library from the input files passed to `ld`, searching
    // for a filename match.
    for input_path in options.input_paths {
        let library_name = OsStr::new(library_name);
        if input_path == library_name || input_path.file_name() == Some(library_name) {
            return Ok(input_path.to_owned());
        }
    }

    // Search for the library from the input files passed to `ld`, searching
    // for an ELF file with a matching `DT_SONAME` section.
    for input_path in options.input_paths {
        if let Ok(bytes) = std::fs::read(input_path) {
            if let Ok(elf) = goblin::elf::Elf::parse(&bytes) {
                if elf.soname == Some(library_name) {
                    return Ok(input_path.to_owned());
                }
            }
        }
    }

    Err(AutowrapError::LibraryNotFound(library_name.to_string()))
}

#[derive(Debug, thiserror::Error)]
pub enum AutowrapError {
    #[error("invalid path")]
    InvalidPath,
    #[error("unsupported interpreter path: {0}")]
    UnsupportedInterpreterPath(String),
    #[error("{0}")]
    IoError(#[from] std::io::Error),
    #[error("{0}")]
    GoblinError(#[from] goblin::error::Error),
    #[error("error when finding resource dir")]
    ResourceDirError(#[from] crate::PackResourceDirError),
    #[error("library not found in search paths: {0}")]
    LibraryNotFound(String),
    #[error("error writing packed program")]
    InjectPackError(#[from] crate::InjectPackError),
    #[error("error adding blob: {0}")]
    AddBlobError(#[from] crate::resources::AddBlobError),
}

fn is_path_executable(path: &Path) -> std::io::Result<bool> {
    use std::os::unix::prelude::PermissionsExt as _;

    let metadata = std::fs::metadata(path)?;

    let permissions = metadata.permissions();
    let mode = permissions.mode();
    let is_executable = mode & 0o111 != 0;

    Ok(is_executable)
}
