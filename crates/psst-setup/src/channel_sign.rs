use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signer, SigningKey};
use psst_setup::{ChannelManifest, UPDATE_KEY_ID, canonical_payload, validate_manifest};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use zeroize::Zeroize;

const MAX_BINARY_BYTES: usize = 128 * 1024 * 1024;

struct Arguments {
    version: String,
    revision: String,
    publication_run: String,
    psst_url: String,
    psst: PathBuf,
    output: PathBuf,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("channel signing failed: {error}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_arguments(std::env::args_os().skip(1))?;
    let binary_size = fs::metadata(&args.psst).map_err(fixed_error)?.len();
    if binary_size > MAX_BINARY_BYTES as u64 {
        return Err("psst input is not a bounded Windows executable".to_owned());
    }
    let capacity = usize::try_from(binary_size)
        .map_err(|_| "psst input is not a bounded Windows executable".to_owned())?;
    let mut binary = Vec::with_capacity(capacity);
    fs::File::open(&args.psst)
        .map_err(fixed_error)?
        .take(MAX_BINARY_BYTES as u64 + 1)
        .read_to_end(&mut binary)
        .map_err(fixed_error)?;
    if binary.len() > MAX_BINARY_BYTES || !binary.starts_with(b"MZ") {
        return Err("psst input is not a bounded Windows executable".to_owned());
    }
    let mut encoded_private = std::env::var("PSST_UPDATE_SIGNING_KEY")
        .map_err(|_| "PSST_UPDATE_SIGNING_KEY is unavailable".to_owned())?;
    let decoded = BASE64
        .decode(&encoded_private)
        .map_err(|_| "PSST_UPDATE_SIGNING_KEY is malformed".to_owned())?;
    encoded_private.zeroize();
    let mut private: [u8; 32] = decoded
        .try_into()
        .map_err(|_| "PSST_UPDATE_SIGNING_KEY has the wrong length".to_owned())?;
    let signing_key = SigningKey::from_bytes(&private);
    private.fill(0);
    let mut manifest = ChannelManifest {
        schema: "psst.install-channel.v1".to_owned(),
        version: args.version,
        revision: args.revision,
        target: "windows-x86_64".to_owned(),
        key_id: UPDATE_KEY_ID.to_owned(),
        publication_run: args.publication_run,
        psst_url: args.psst_url,
        psst_bytes: binary.len() as u64,
        psst_sha256: format!("{:x}", Sha256::digest(&binary)),
        signature: String::new(),
    };
    manifest.signature = BASE64.encode(signing_key.sign(&canonical_payload(&manifest)).to_bytes());
    validate_manifest(&manifest).map_err(|error| error.to_string())?;
    let output = serde_json::to_vec_pretty(&manifest).map_err(fixed_error)?;
    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent).map_err(fixed_error)?;
    }
    fs::write(args.output, [output, b"\n".to_vec()].concat()).map_err(fixed_error)
}

fn parse_arguments(
    arguments: impl Iterator<Item = std::ffi::OsString>,
) -> Result<Arguments, String> {
    let mut version = None;
    let mut revision = None;
    let mut publication_run = None;
    let mut psst_url = None;
    let mut psst = None;
    let mut output = None;
    let mut values = arguments.peekable();
    while let Some(flag) = values.next() {
        let value = values
            .next()
            .ok_or_else(|| "every signing argument requires one value".to_owned())?;
        match flag.to_str() {
            Some("--version") => version = value.into_string().ok(),
            Some("--revision") => revision = value.into_string().ok(),
            Some("--publication-run") => publication_run = value.into_string().ok(),
            Some("--psst-url") => psst_url = value.into_string().ok(),
            Some("--psst") => psst = Some(PathBuf::from(value)),
            Some("--output") => output = Some(PathBuf::from(value)),
            _ => return Err("unknown channel signing argument".to_owned()),
        }
    }
    Ok(Arguments {
        version: version.ok_or_else(|| "--version is required".to_owned())?,
        revision: revision.ok_or_else(|| "--revision is required".to_owned())?,
        publication_run: publication_run
            .ok_or_else(|| "--publication-run is required".to_owned())?,
        psst_url: psst_url.ok_or_else(|| "--psst-url is required".to_owned())?,
        psst: psst.ok_or_else(|| "--psst is required".to_owned())?,
        output: output.ok_or_else(|| "--output is required".to_owned())?,
    })
}

fn fixed_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}
