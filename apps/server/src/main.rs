//! Server entry point for `termstage`.

mod assets;
mod cli;
mod local_terminal;
pub mod tunnel_ws;
mod web;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cli::run().await
}
