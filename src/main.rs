use clap::Parser;

#[tokio::main]
async fn main() {
    let cli = sds::cli::Cli::parse();
    if let Err(e) = sds::cli::main(cli).await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
