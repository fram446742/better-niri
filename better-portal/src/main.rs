use std::future::pending;

fn main() -> anyhow::Result<()> {
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    async_io::block_on(run())
}

async fn run() -> anyhow::Result<()> {
    let _connection = better_portal::serve().await?;
    pending::<()>().await;
    Ok(())
}
