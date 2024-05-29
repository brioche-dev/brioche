use std::{path::PathBuf, process::ExitCode};

use bstr::ByteSlice as _;

enum Mode {
    AutowrapEnabled {
        resource_dir: PathBuf,
        all_resource_dirs: Vec<PathBuf>,
    },
    AutowrapDisabled,
}

fn main() -> ExitCode {
    let result = run();

    match result {
        Ok(exit_code) => exit_code,
        Err(err) => {
            eprintln!("{:#}", err);
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, LdError> {
    let current_exe = std::env::current_exe().map_err(LdError::FailedToGetCurrentExe)?;
    let current_exe_dir = current_exe
        .parent()
        .ok_or_else(|| LdError::FailedToGetCurrentExeDir)?;
    let current_exe_parent_dir = current_exe_dir
        .parent()
        .ok_or_else(|| LdError::FailedToGetCurrentExeDir)?;
    let ld_resource_dir = current_exe_parent_dir.join("libexec").join("brioche-ld");
    if !ld_resource_dir.is_dir() {
        return Err(LdError::LinkerResourceDirNotFound(ld_resource_dir));
    }

    let linker = ld_resource_dir.join("ld");
    let packed_path = ld_resource_dir.join("brioche-packed");

    let mut output_path = PathBuf::from("a.out");
    let mut library_search_paths = vec![];
    let mut input_paths = vec![];

    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        let arg = <[u8]>::from_os_str(&arg).ok_or_else(|| LdError::InvalidArg)?;
        let arg = bstr::BStr::new(arg);

        if &**arg == b"-o" {
            let output = args.next().ok_or_else(|| LdError::InvalidArg)?;
            output_path = PathBuf::from(output);
        } else if let Some(output) = arg.strip_prefix(b"-o") {
            let output = output.to_path().map_err(|_| LdError::InvalidPath)?;
            output_path = output.to_owned();
        } else if &**arg == b"-L" {
            let lib_path = args.next().ok_or_else(|| LdError::InvalidArg)?;
            library_search_paths.push(PathBuf::from(lib_path));
        } else if let Some(lib_path) = arg.strip_prefix(b"-L") {
            let lib_path = lib_path.to_path().map_err(|_| LdError::InvalidPath)?;
            library_search_paths.push(lib_path.to_owned());
        } else if arg.starts_with(b"-") {
            // Ignore other arguments
        } else {
            let input_path = arg.to_path().map_err(|_| LdError::InvalidPath)?;
            input_paths.push(input_path.to_owned());
        }
    }

    // Determine whether we will wrap the resulting binary or not. We do this
    // before running the command so we can bail early if the resource dir
    // cannot be found.
    let autowrap_mode = match std::env::var("BRIOCHE_LD_AUTOWRAP").as_deref() {
        Ok("false") => Mode::AutowrapDisabled,
        _ => {
            let resource_dir = brioche_pack::find_output_resource_dir(&output_path)
                .map_err(LdError::ResourceDirError)?;
            let all_resource_dirs = brioche_pack::find_resource_dirs(&current_exe, true)
                .map_err(LdError::ResourceDirError)?;
            Mode::AutowrapEnabled {
                resource_dir,
                all_resource_dirs,
            }
        }
    };
    let skip_unknown_libs = matches!(
        std::env::var("BRIOCHE_LD_AUTOWRAP_SKIP_UNKNOWN_LIBS").as_deref(),
        Ok("true")
    );

    let mut command = std::process::Command::new(linker);
    command.args(std::env::args_os().skip(1));
    let status = command.status()?;

    if !status.success() {
        let exit_code = status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .map(ExitCode::from)
            .unwrap_or(ExitCode::FAILURE);
        return Ok(exit_code);
    }

    match autowrap_mode {
        Mode::AutowrapEnabled {
            resource_dir,
            all_resource_dirs,
        } => {
            brioche_pack::autowrap::autowrap(brioche_pack::autowrap::AutowrapOptions {
                program_path: &output_path,
                packed_exec_path: &packed_path,
                resource_dir: &resource_dir,
                all_resource_dirs: &all_resource_dirs,
                sysroot: &ld_resource_dir,
                library_search_paths: &library_search_paths,
                input_paths: &input_paths,
                skip_libs: &[],
                skip_unknown_libs,
                runtime_library_dirs: &[],
            })?;
        }
        Mode::AutowrapDisabled => {
            // We already wrote the binary, so nothing to do
        }
    };

    Ok(ExitCode::SUCCESS)
}

#[derive(Debug, thiserror::Error)]
enum LdError {
    #[error("error wrapping binary: {0}")]
    AutowrapError(#[from] brioche_pack::autowrap::AutowrapError),
    #[error("invalid arg")]
    InvalidArg,
    #[error("invalid path")]
    InvalidPath,
    #[error("failed to find current executable: {0}")]
    FailedToGetCurrentExe(#[source] std::io::Error),
    #[error("failed to get directory containing current executable")]
    FailedToGetCurrentExeDir,
    #[error("brioche-ld resource dir not found (expected to be at {0})")]
    LinkerResourceDirNotFound(PathBuf),
    #[error("{0}")]
    IoError(#[from] std::io::Error),
    #[error("{0}")]
    GoblinError(#[from] goblin::error::Error),
    #[error("error when finding resource dir")]
    ResourceDirError(#[from] brioche_pack::PackResourceDirError),
    #[error("error writing packed program")]
    InjectPackError(#[from] brioche_pack::InjectPackError),
    #[error("error adding blob: {0}")]
    AddBlobError(#[from] brioche_pack::resources::AddBlobError),
}
