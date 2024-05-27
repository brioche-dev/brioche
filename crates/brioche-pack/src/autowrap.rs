use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use bstr::ByteSlice as _;

#[derive(Debug, Clone, Copy)]
pub struct AutowrapOptions<'a> {
    pub program_path: &'a Path,
    pub packed_exec_path: &'a Path,
    pub resource_dir: &'a Path,
    pub sysroot: &'a Path,
    pub library_search_paths: &'a [PathBuf],
    pub input_paths: &'a [PathBuf],
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
                    interpreter_path: &interpreter_path,
                    library_search_paths: options.library_search_paths,
                    input_paths: options.input_paths,
                    elf: &elf,
                })?;

                let mut packed = std::fs::File::open(options.packed_exec_path)?;
                let mut packed_program = std::fs::File::create(options.program_path)?;
                std::io::copy(&mut packed, &mut packed_program)?;

                crate::inject_pack(&mut packed_program, &pack)?;
            } else {
                // Output is statically-linked, nothing to do
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
    interpreter_path: &'a Path,
    library_search_paths: &'a [PathBuf],
    input_paths: &'a [PathBuf],
    elf: &'a goblin::elf::Elf<'a>,
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
        program_name,
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
        interpreter_name,
    )?;
    let resource_interpreter_path = <[u8]>::from_path(&resource_interpreter_path)
        .ok_or_else(|| AutowrapError::InvalidPath)?
        .into();

    let mut resource_library_dirs = vec![];
    for library_name in &options.elf.libraries {
        let library_path = find_library(&options, library_name)?;

        let library = std::fs::File::open(&library_path)?;
        let resource_library_path = crate::resources::add_named_blob(
            options.resource_dir,
            library,
            is_path_executable(&library_path)?,
            library_name,
        )?;
        let resource_library_dir = resource_library_path
            .parent()
            .expect("no parent dir for library path");
        let resource_library_dir = <[u8]>::from_path(resource_library_dir)
            .ok_or_else(|| AutowrapError::InvalidPath)?
            .into();

        resource_library_dirs.push(resource_library_dir);
    }

    let interpreter = crate::Interpreter::LdLinux {
        path: resource_interpreter_path,
        library_paths: resource_library_dirs,
    };
    let resource_program_path = <[u8]>::from_path(&resource_program_path)
        .ok_or_else(|| AutowrapError::InvalidPath)?
        .into();
    let pack = crate::Pack {
        program: resource_program_path,
        interpreter: Some(interpreter),
    };

    Ok(pack)
}

fn find_library(
    options: &DynamicLdLinuxElfPackOptions,
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
    ResourcesDirError(#[from] crate::PackResourceDirError),
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
