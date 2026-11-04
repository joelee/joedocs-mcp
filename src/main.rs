//! Binary entry point: a thin wrapper around the library.

use std::path::Path;
use std::process::ExitCode;

use clap::Parser as _;

use joedocs::auth::OsRandom;
use joedocs::clock::SystemClock;
use joedocs::env::ProcessEnv;
use joedocs::fs::RealFs;
use joedocs::{AppId, Cli, Context, EXIT_CONFIG, Error};

fn main() -> ExitCode {
    let cli = Cli::parse();

    let env = match ProcessEnv::with_dotenv_file(Path::new(".env")) {
        Ok(env) => env,
        Err(error) => {
            eprintln!("error: cannot read .env: {error}");
            return ExitCode::from(EXIT_CONFIG);
        }
    };
    let ctx = Context {
        app: AppId::current(),
        env: &env,
        fs: &RealFs,
        clock: std::sync::Arc::new(SystemClock),
        random: &OsRandom,
    };

    let loaded = match joedocs::load(&cli, &ctx) {
        Ok(loaded) => loaded,
        Err(error) => return fail(&error),
    };

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(loaded.config().log.level.as_filter())
        .init();

    let mut stdout = std::io::stdout().lock();
    match joedocs::execute(&cli, &loaded, &ctx, &mut stdout) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(&error),
    }
}

fn fail(error: &Error) -> ExitCode {
    eprintln!("error: {error}");
    ExitCode::from(error.exit_code())
}
