use std::{
    ffi::{CStr, CString},
    process::ExitCode,
};

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
            let interpreter = interpreter
                .to_path()
                .map_err(|_| PackedError::InvalidPath)?;
            let interpreter = resource_dir.join(interpreter);

            let program = pack
                .program
                .to_path()
                .map_err(|_| PackedError::InvalidPath)?;
            let program = resource_dir.join(program).canonicalize()?;
            let mut exec = userland_execve::ExecOptions::new(&interpreter);

            let interpreter =
                <[u8]>::from_path(&interpreter).ok_or_else(|| PackedError::InvalidPath)?;
            let interpreter = CString::new(interpreter).map_err(|_| PackedError::InvalidPath)?;

            // Add argv0
            exec.arg(interpreter);

            let vars = std::env::vars_os()
                .map(|(key, value)| {
                    let key =
                        <[u8]>::from_os_str(&key).ok_or_else(|| PackedError::InvalidEnvVar)?;
                    let key = CString::new(key).map_err(|_| PackedError::InvalidEnvVar)?;
                    let value =
                        <[u8]>::from_os_str(&value).ok_or_else(|| PackedError::InvalidEnvVar)?;
                    let value = CString::new(value).map_err(|_| PackedError::InvalidEnvVar)?;
                    Ok::<_, PackedError>((key, value))
                })
                .collect::<Result<Vec<_>, _>>()?;
            exec.envs(vars);

            let mut args = std::env::args_os();

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

                exec.arg(CStr::from_bytes_with_nul(b"--library-path\0").unwrap());

                let ld_library_path =
                    CString::new(ld_library_path).map_err(|_| PackedError::InvalidPath)?;
                exec.arg(ld_library_path);
            }

            if let Some(arg0) = args.next() {
                exec.arg(CStr::from_bytes_with_nul(b"--argv0\0").unwrap());
                let arg0 = <[u8]>::from_os_str(&arg0).ok_or_else(|| PackedError::InvalidPath)?;
                let arg0 = CString::new(arg0).map_err(|_| PackedError::InvalidPath)?;
                exec.arg(arg0);
            }

            let program = <[u8]>::from_path(&program).ok_or_else(|| PackedError::InvalidPath)?;
            let program = CString::new(program).map_err(|_| PackedError::InvalidPath)?;
            exec.arg(program);

            let args = args
                .map(|arg| {
                    let arg =
                        <[u8]>::from_os_str(&arg).ok_or_else(|| PackedError::InvalidEnvVar)?;
                    let arg = CString::new(arg).map_err(|_| PackedError::InvalidEnvVar)?;
                    Ok::<_, PackedError>(arg)
                })
                .collect::<Result<Vec<_>, _>>()?;
            exec.args(args);

            userland_execve::exec_with_options(exec);
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
    #[error("invalid env var")]
    InvalidEnvVar,
}
