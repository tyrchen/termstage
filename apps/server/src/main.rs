//! Server entry point for `presenterm`.

mod assets;
mod cli;
mod web;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cli::run().await
}
