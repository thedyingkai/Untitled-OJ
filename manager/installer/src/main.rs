use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    ojos_orchestrator_installer::run(ojos_orchestrator_installer::Cli::parse())
}
