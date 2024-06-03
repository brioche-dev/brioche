use std::{os::unix::process::CommandExt as _, process::ExitCode};

use bstr::ByteSlice as _;

const BRIOCHE_PACKED_ERROR: u8 = 121;

pub fn main() -> ExitCode {
    let result = run();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("brioche-packed error: {err}");
            ExitCode::from(BRIOCHE_PACKED_ERROR)
        }
    }
}

fn run() -> Result<(), PackedError> {
    let path = std::env::current_exe()?;
    let parent_path = path.parent().ok_or(PackedError::InvalidPath)?;
    let resource_dirs = brioche_pack::find_resource_dirs(&path, true)?;
    let mut program = std::fs::File::open(&path)?;
    let pack = brioche_pack::extract_pack(&mut program)?;

    match pack {
        brioche_pack::Pack::LdLinux {
            program,
            interpreter,
            library_dirs,
            runtime_library_dirs,
        } => {
            let mut args = std::env::args_os();

            let interpreter = interpreter
                .to_path()
                .map_err(|_| PackedError::InvalidPath)?;
            let interpreter = brioche_pack::find_in_resource_dirs(&resource_dirs, interpreter)
                .ok_or(PackedError::ResourceNotFound)?;
            let mut command = std::process::Command::new(interpreter);

            let mut resolved_library_dirs = vec![];

            for library_dir in &runtime_library_dirs {
                let library_dir = library_dir
                    .to_path()
                    .map_err(|_| PackedError::InvalidPath)?;
                let resolved_library_dir = parent_path.join(library_dir);
                resolved_library_dirs.push(resolved_library_dir);
            }

            for library_dir in &library_dirs {
                let library_dir = library_dir
                    .to_path()
                    .map_err(|_| PackedError::InvalidPath)?;
                let library_dir = brioche_pack::find_in_resource_dirs(&resource_dirs, library_dir)
                    .ok_or(PackedError::ResourceNotFound)?;
                resolved_library_dirs.push(library_dir);
            }

            if !resolved_library_dirs.is_empty() {
                let mut ld_library_path = bstr::BString::default();
                for (n, library_dir) in resolved_library_dirs.iter().enumerate() {
                    if n > 0 {
                        ld_library_path.push(b':');
                    }

                    let path =
                        <[u8]>::from_path(library_dir).ok_or_else(|| PackedError::InvalidPath)?;
                    ld_library_path.extend(path);
                }

                if let Some(env_library_path) = std::env::var_os("LD_LIBRARY_PATH") {
                    let env_library_path = <[u8]>::from_os_str(&env_library_path)
                        .ok_or_else(|| PackedError::InvalidPath)?;
                    if !env_library_path.is_empty() {
                        ld_library_path.push(b':');
                        ld_library_path.extend(env_library_path);
                    }
                }

                command.arg("--library-path");

                let ld_library_path = ld_library_path
                    .to_os_str()
                    .map_err(|_| PackedError::InvalidPath)?;
                command.arg(ld_library_path);
            }

            if let Some(arg0) = args.next() {
                command.arg("--argv0");
                command.arg(arg0);
            }

            let program = program.to_path().map_err(|_| PackedError::InvalidPath)?;
            let program = brioche_pack::find_in_resource_dirs(&resource_dirs, program)
                .ok_or(PackedError::ResourceNotFound)?;
            let program = program.canonicalize()?;
            command.arg(program);

            command.args(args);

            let error = command.exec();
            Err(PackedError::IoError(error))
        }
        brioche_pack::Pack::Static { .. } => {
            unimplemented!("execution of a static executable");
        }
        brioche_pack::Pack::Metadata { .. } => {
            unimplemented!("execution of a metadata pack");
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum PackedError {
    IoError(#[from] std::io::Error),
    ExtractPackError(#[from] brioche_pack::ExtractPackError),
    PackResourceDirError(#[from] brioche_pack::PackResourceDirError),
    ResourceNotFound,
    InvalidPath,
}

impl std::fmt::Display for PackedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(error_summary(self))
    }
}

fn error_summary(error: &PackedError) -> &'static str {
    match error {
        PackedError::IoError(_) => "io error",
        PackedError::ExtractPackError(error) => match error {
            brioche_pack::ExtractPackError::ReadPackedProgramError(_) => {
                "failed to read packed program: io error"
            }
            brioche_pack::ExtractPackError::MarkerNotFound => {
                "marker not found at the end of the packed program"
            }
            brioche_pack::ExtractPackError::MalformedMarker => {
                "malformed marker at the end of the packed program"
            }
            brioche_pack::ExtractPackError::InvalidPack(_) => "failed to parse pack: bincode error",
        },
        PackedError::PackResourceDirError(error) => match error {
            brioche_pack::PackResourceDirError::NotFound => "brioche pack resource dir not found",
            brioche_pack::PackResourceDirError::DepthLimitReached => {
                "reached depth limit while searching for brioche pack resource dir"
            }
            brioche_pack::PackResourceDirError::IoError(_) => {
                "error while searching for brioche pack resource dir: io error"
            }
        },
        PackedError::ResourceNotFound => "resource not found",
        PackedError::InvalidPath => "invalid path",
    }
}
