use std::{os::unix::process::CommandExt as _, process::ExitCode};

use bstr::ByteSlice as _;

const BRIOCHE_PACKED_ERROR: u8 = 121;

fn main() -> ExitCode {
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
    let resource_dir = brioche_pack::find_resource_dir(&path)?;
    let mut program = std::fs::File::open(&path)?;
    let pack = brioche_pack::extract_pack(&mut program)?;

    match pack.interpreter {
        Some(brioche_pack::Interpreter::LdLinux {
            path: interpreter,
            library_paths,
        }) => {
            let mut args = std::env::args_os();

            let interpreter = interpreter
                .to_path()
                .map_err(|_| PackedError::InvalidPath)?;
            let interpreter = resource_dir.join(interpreter);
            let mut command = std::process::Command::new(interpreter);
            if !library_paths.is_empty() {
                let mut ld_library_path = bstr::BString::default();
                for (n, library_path) in library_paths.iter().enumerate() {
                    let library_path = library_path
                        .to_path()
                        .map_err(|_| PackedError::InvalidPath)?;
                    let library_path = resource_dir.join(library_path);

                    if n > 0 {
                        ld_library_path.push(b':');
                    }

                    let path =
                        <[u8]>::from_path(&library_path).ok_or_else(|| PackedError::InvalidPath)?;
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

            let program = pack
                .program
                .to_path()
                .map_err(|_| PackedError::InvalidPath)?;
            let program = resource_dir.join(program).canonicalize()?;
            command.arg(program);

            command.args(args);

            let error = command.exec();
            Err(PackedError::IoError(error))
        }
        None => {
            unimplemented!("execution without an interpreter");
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum PackedError {
    #[error("{0}")]
    IoError(#[from] std::io::Error),
    #[error("{0}")]
    ExtractPackError(#[from] brioche_pack::ExtractPackError),
    #[error("{0}")]
    PackResourceDirError(#[from] brioche_pack::PackResourceDirError),
    #[error("invalid path")]
    InvalidPath,
}
