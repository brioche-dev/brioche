use std::{path::PathBuf, process::ExitCode};

use clap::Parser;

#[derive(Debug, Parser)]
enum Args {
    Pack {
        #[clap(long)]
        packed: PathBuf,
        #[clap(long)]
        output: PathBuf,
        #[clap(long)]
        pack: String,
    },
    Autowrap {
        #[clap(long)]
        packed_exec: PathBuf,
        #[clap(long)]
        sysroot: PathBuf,
        #[clap(short = 'L', long = "lib-dir")]
        lib_dirs: Vec<PathBuf>,
        programs: Vec<PathBuf>,
    },
    Read {
        program: PathBuf,
    },
}

fn main() -> ExitCode {
    let result = run();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), PackerError> {
    let args = Args::parse();

    match args {
        Args::Pack {
            packed,
            output,
            pack,
        } => {
            let pack = serde_json::from_str(&pack).map_err(PackerError::DeserializePack)?;

            std::fs::copy(packed, &output)?;
            let mut output = std::fs::OpenOptions::new().append(true).open(&output)?;

            brioche_pack::inject_pack(&mut output, &pack)?;
        }
        Args::Autowrap {
            packed_exec,
            sysroot,
            lib_dirs,
            programs,
        } => {
            for program in &programs {
                let resources_dir = brioche_pack::find_resource_dir(program).map_err(|error| {
                    PackerError::PackResourceDir {
                        program: program.clone(),
                        error,
                    }
                })?;
                brioche_pack::autowrap::autowrap(brioche_pack::autowrap::AutowrapOptions {
                    program_path: program,
                    packed_exec_path: &packed_exec,
                    resources_dir: &resources_dir,
                    library_search_paths: &lib_dirs,
                    input_paths: &[],
                    sysroot: &sysroot,
                })
                .map_err(|error| PackerError::Autowrap {
                    program: program.clone(),
                    error,
                })?;
            }
        }
        Args::Read { program } => {
            let mut program = std::fs::File::open(program)?;
            let pack = brioche_pack::extract_pack(&mut program)?;

            serde_json::to_writer_pretty(std::io::stdout().lock(), &pack)
                .map_err(PackerError::SerializePack)?;
            println!();
        }
    }

    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum PackerError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("error deserializing pack: {0}")]
    DeserializePack(#[source] serde_json::Error),
    #[error("error serializing pack: {0}")]
    SerializePack(#[source] serde_json::Error),
    #[error("{0}")]
    InjectPack(#[from] brioche_pack::InjectPackError),
    #[error("{0}")]
    ExtractPack(#[from] brioche_pack::ExtractPackError),
    #[error("error wrapping {program}: {error}")]
    PackResourceDir {
        program: PathBuf,
        #[source]
        error: brioche_pack::PackResourceDirError,
    },
    #[error("error wrapping {program}: {error}")]
    Autowrap {
        program: PathBuf,
        #[source]
        error: brioche_pack::autowrap::AutowrapError,
    },
}
