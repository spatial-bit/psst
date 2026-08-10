use psst_codex::start_from_environment;

#[tokio::main]
async fn main() {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.as_slice() == ["--version"] {
        println!("psst-codex {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if !arguments.is_empty() {
        eprintln!("psst-codex: unexpected command-line arguments");
        std::process::exit(64);
    }
    let Ok(activation) = start_from_environment().await else {
        eprintln!("psst-codex: activation startup failed");
        std::process::exit(70);
    };
    if tokio::signal::ctrl_c().await.is_err() || activation.shutdown().await.is_err() {
        eprintln!("psst-codex: activation shutdown failed");
        std::process::exit(70);
    }
}
