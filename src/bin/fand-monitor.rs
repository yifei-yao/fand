fn main() {
    let args: Vec<String> = std::env::args().collect();
    let config_path = args.get(1).map(String::as_str).unwrap_or("config.toml");

    if let Err(e) = fand::monitor::run(config_path) {
        eprintln!("fand-monitor: {e}");
        std::process::exit(1);
    }
}
