use std::process::ExitCode;

fn main() -> ExitCode {
    match git_relay::cli::run(std::env::args_os()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}
