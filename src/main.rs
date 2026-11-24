//! The composition root: the only place that knows both adapters.

fn main() -> std::process::ExitCode {
    flot::adapters::cli::main(flot::adapters::mcp::serve_stdio)
}
