use clap::Parser;
use haste::{run, Cli};

fn main() {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(out) => println!("{out}"),
        Err(e) => {
            eprintln!("haste: {e}");
            std::process::exit(1);
        }
    }
}
