fn main() {
    if let Err(error) = nagi::cli::run_from_env() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
