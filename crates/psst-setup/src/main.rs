use psst_setup::{
    DEFAULT_CHANNEL_URL, InstallOutcome, SetupError, default_install_dir, fetch_binary,
    fetch_channel, install_verified,
};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Debug)]
struct Arguments {
    channel: String,
    install_dir: Option<PathBuf>,
    update_path: bool,
    assume_yes: bool,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("\nPsst setup stopped safely.\n\n{error}\n");
            pause_if_interactive();
            ExitCode::from(1)
        }
    }
}

async fn run() -> Result<(), SetupError> {
    if !cfg!(all(windows, target_arch = "x86_64")) {
        return Err(SetupError::UnsupportedPlatform);
    }
    let arguments = parse_arguments(std::env::args_os().skip(1))?;
    println!("Psst Setup\n==========\n");
    println!("This installs or updates Psst for this Windows account.");
    println!("Relay data, profiles, credentials, and messages are never removed.\n");
    if !arguments.assume_yes && !confirm()? {
        println!("Nothing changed.");
        return Ok(());
    }
    println!("Checking the Psst update channel...");
    let manifest = fetch_channel(&arguments.channel).await?;
    println!(
        "Downloading Psst {} ({})...",
        manifest.version,
        &manifest.revision[..8]
    );
    let binary = fetch_binary(&manifest).await?;
    let install_dir = arguments.install_dir.unwrap_or(default_install_dir()?);
    let result = install_verified(&manifest, &binary, &install_dir, arguments.update_path)?;
    let action = match result.outcome {
        InstallOutcome::Installed => "installed",
        InstallOutcome::Updated => "updated",
        InstallOutcome::AlreadyCurrent => "is already current",
    };
    println!("\nPsst {action}.");
    println!("Version: {}", result.version);
    println!("Revision: {}", result.revision);
    println!("Location: {}", result.install_dir.display());
    if result.path_changed {
        println!("PATH: added for this Windows account; open a new terminal.");
    } else if let Some(warning) = result.path_warning {
        println!("PATH: not changed ({warning})");
        println!(
            "Run Psst directly from {}.",
            result.install_dir.join("psst.exe").display()
        );
    } else {
        println!("PATH: already configured.");
    }
    println!("\nNext: open a new terminal and run `psst --version`.");
    pause_if_interactive();
    Ok(())
}

fn parse_arguments(
    arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<Arguments, SetupError> {
    let mut channel = DEFAULT_CHANNEL_URL.to_owned();
    let mut install_dir = None;
    let mut update_path = true;
    let mut assume_yes = false;
    let mut values = arguments.peekable();
    while let Some(value) = values.next() {
        match value.to_str() {
            Some("--channel") => {
                channel = values
                    .next()
                    .and_then(|value| value.into_string().ok())
                    .ok_or(SetupError::InvalidArguments(
                        "--channel requires one UTF-8 URL.",
                    ))?;
            }
            Some("--install-dir") => {
                install_dir = Some(PathBuf::from(values.next().ok_or(
                    SetupError::InvalidArguments("--install-dir requires one path."),
                )?));
            }
            Some("--no-path") => update_path = false,
            Some("--yes") => assume_yes = true,
            Some("--version") => {
                println!("psst-setup {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            _ => {
                return Err(SetupError::InvalidArguments(
                    "Usage: psst-setup [--yes] [--no-path]",
                ));
            }
        }
    }
    Ok(Arguments {
        channel,
        install_dir,
        update_path,
        assume_yes,
    })
}

fn confirm() -> Result<bool, SetupError> {
    print!("Continue? [Y/n] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "" | "y" | "yes"
    ))
}

fn pause_if_interactive() {
    if std::env::args_os().any(|argument| argument == "--yes") {
        return;
    }
    print!("\nPress Enter to close...");
    let _ = io::stdout().flush();
    let mut ignored = String::new();
    let _ = io::stdin().read_line(&mut ignored);
}
