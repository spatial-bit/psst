use psst_mcp::serve_bounded;

#[tokio::main]
async fn main() {
    if serve_bounded(tokio::io::stdin(), tokio::io::stdout())
        .await
        .is_err()
    {
        eprintln!("psst-mcp: protocol session failed");
        std::process::exit(70);
    }
}
