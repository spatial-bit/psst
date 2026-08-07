fn main() {
    let document = psst_protocol::openapi_document();
    if let Some(path) = std::env::args_os().nth(1) {
        std::fs::write(path, document).expect("write OpenAPI document");
    } else {
        print!("{document}");
    }
}
