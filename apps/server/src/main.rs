//! Server entry point for `termstage`.

mod assets;
mod cli;
pub mod tunnel_ws;
mod web;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cli::run().await
}
