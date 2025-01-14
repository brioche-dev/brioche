use std::process::ExitCode;

use brioche_core::sandbox::{SandboxBackend, SandboxExecutionConfig};
use clap::Parser;

const BRIOCHE_SANDBOX_ERROR_CODE: u8 = 122;

#[derive(Debug, Parser)]
pub struct RunSandboxArgs {
    #[arg(long)]
    backend: String,
    #[arg(long)]
    config: String,
}

pub fn run_sandbox(args: RunSandboxArgs) -> ExitCode {
    let backend = match serde_json::from_str::<SandboxBackend>(&args.backend) {
        Ok(backend) => backend,
        Err(error) => {
            eprintln!("brioche: invalid backend JSON: {error:#}");
            return ExitCode::from(BRIOCHE_SANDBOX_ERROR_CODE);
        }
    };
    let config = match serde_json::from_str::<SandboxExecutionConfig>(&args.config) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("brioche: failed to parse sandbox config: {error:#}");
            return ExitCode::from(BRIOCHE_SANDBOX_ERROR_CODE);
        }
    };

    let status = match brioche_core::sandbox::run_sandbox(backend, config) {
        Ok(status) => status,
        Err(error) => {
            eprintln!("brioche: failed to run sandbox: {error:#}");
            return ExitCode::from(BRIOCHE_SANDBOX_ERROR_CODE);
        }
    };

    status
        .code()
        .and_then(|code| {
            let code: u8 = code.try_into().ok()?;
            Some(ExitCode::from(code))
        })
        .unwrap_or_else(|| {
            if status.success() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        })
}
