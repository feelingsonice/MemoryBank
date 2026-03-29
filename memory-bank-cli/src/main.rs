fn main() {
    if let Err(error) = memory_bank_cli::run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
