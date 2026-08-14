use psst_mcp::{ConfiguredStdioError, serve_configured_stdio};

#[tokio::main]
async fn main() {
    match serve_configured_stdio().await {
        Ok(()) => {}
        Err(ConfiguredStdioError::Startup) => {
            eprintln!("psst-mcp: cooperative session startup failed");
            std::process::exit(70);
        }
        Err(ConfiguredStdioError::Protocol) => {
            eprintln!("psst-mcp: protocol session failed");
            std::process::exit(70);
        }
    }
}
