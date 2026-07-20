fn main() {
    let args: Vec<String> = std::env::args().collect();

    let result = match args.get(1).map(String::as_str) {
        Some("setup") => fand::setup::interactive(),
        Some("run") => fand::daemon::run(args.get(2).map(String::as_str).unwrap_or("config.toml")),
        Some("test") => fand::manual::interactive(),
        _ => {
            eprintln!("usage: fand setup > config.toml");
            eprintln!("       fand run [config.toml]");
            eprintln!("       fand test");
            std::process::exit(2);
        }
    };

    if let Err(e) = result {
        eprintln!("fand: {e}");
        std::process::exit(1);
    }
}
