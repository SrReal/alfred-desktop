#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 2 && args[1] == "--window-worker" {
        std::process::exit(alfredpet_lib::window_worker_entry());
    }
    if args.get(1).is_some_and(|arg| arg == "--admin-worker") {
        std::process::exit(alfredpet_lib::admin::entry(&args));
    }
    if std::env::args().any(|a| a == "--mcp-bridge") {
        alfredpet_lib::mcp_client();
    } else {
        alfredpet_lib::run();
    }
}
