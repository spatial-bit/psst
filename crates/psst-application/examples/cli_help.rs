fn main() {
    std::fs::write(
        std::env::args_os().nth(1).expect("destination"),
        psst_application::CLI_HELP,
    )
    .unwrap();
}
