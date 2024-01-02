#![cfg_attr(not(test), no_main)]

use std::ffi::{CStr, CString};

use bstr::ByteSlice as _;

const BRIOCHE_PACKED_ERROR: u8 = 121;

extern "C" {
    static environ: *const *const libc::c_char;
}

#[cfg_attr(not(test), no_mangle)]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn main(argc: libc::c_int, argv: *const *const libc::c_char) -> libc::c_int {
    let mut args = vec![];
    let mut env_vars = vec![];

    let argc: usize = argc.try_into().unwrap_or(0);
    for n in 0..argc {
        let arg = unsafe { *argv.add(n) };

        if arg.is_null() {
            break;
        }

        let arg = unsafe { CStr::from_ptr(arg) };
        args.push(arg);
    }

    for n in 0.. {
        let var = unsafe { *environ.add(n) };

        if var.is_null() {
            break;
        }

        let var = unsafe { CStr::from_ptr(var) };
        env_vars.push(var);
    }

    let result = run(&args, &env_vars);
    match result {
        Ok(()) => libc::EXIT_SUCCESS,
        Err(err) => {
            eprintln!("brioche-packed error: {err}");
            BRIOCHE_PACKED_ERROR.into()
        }
    }
}

fn run(args: &[&CStr], env_vars: &[&CStr]) -> Result<(), PackedError> {
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

            let mut args = args.iter();
            if let Some(arg0) = args.next() {
                exec.arg(CStr::from_bytes_with_nul(b"--argv0\0").unwrap());
                exec.arg(arg0);
            }

            let program = <[u8]>::from_path(&program).ok_or_else(|| PackedError::InvalidPath)?;
            let program = CString::new(program).map_err(|_| PackedError::InvalidPath)?;
            exec.arg(program);

            exec.args(args);

            exec.env_pairs(env_vars);

            userland_execve::exec_with_options(exec);
        }
        None => {
            unimplemented!("execution without an interpreter");
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum PackedError {
    IoError(#[from] std::io::Error),
    ExtractPackError(#[from] brioche_pack::ExtractPackError),
    PackResourceDirError(#[from] brioche_pack::PackResourceDirError),
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
        PackedError::InvalidPath => "invalid path",
    }
}
