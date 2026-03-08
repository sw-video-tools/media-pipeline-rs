use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "operator-cli")]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    Submit { file: String },
    Status { job_id: String },
}

fn main() {
    let cli = Cli::parse();
    match cli.cmd {
        Command::Submit { file } => println!("submit stub: {file}"),
        Command::Status { job_id } => println!("status stub: {job_id}"),
    }
}
