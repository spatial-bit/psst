use psst_application::tool_contracts;

fn main() {
    let tools = tool_contracts();
    let text = serde_json::to_string_pretty(&tools).unwrap() + "\n";
    std::fs::write(std::env::args_os().nth(1).expect("destination"), text).unwrap();
}
