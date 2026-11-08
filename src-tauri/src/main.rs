// Release builds use the GUI subsystem on Windows (no console window). Stdio pipes passed
// by an MCP client are still inherited, so `--mcp-stdio` works from Claude Code and Hermes.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--memory-quality") {
        std::process::exit(agentos_studio_lib::memory_quality::run_from_args(&args));
    }
    if args.iter().any(|a| a == "--mcp-stdio" || a == "--check") {
        std::process::exit(agentos_studio_lib::mcp_stdio::run_from_args(&args));
    }
    agentos_studio_lib::run_gui();
}
