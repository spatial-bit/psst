use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    psst_cli::run_process(std::env::args_os()).await
}
