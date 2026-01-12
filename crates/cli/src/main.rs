use clap::Parser;

#[tokio::main]
async fn main() {
    if let Err(err) = perpl_cli::run(perpl_cli::args::Cli::parse()).await {
        eprintln!("Error: {:#}", err);
        std::process::exit(1);
    }
}
