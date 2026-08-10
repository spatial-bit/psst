use psst_mcp::{CooperativeServer, serve_bounded_with};

#[tokio::main]
async fn main() {
    let Ok(server) = CooperativeServer::from_environment().await else {
        eprintln!("psst-mcp: cooperative session startup failed");
        std::process::exit(70);
    };
    if serve_bounded_with(server, tokio::io::stdin(), tokio::io::stdout())
        .await
        .is_err()
    {
        eprintln!("psst-mcp: protocol session failed");
        std::process::exit(70);
    }
}
