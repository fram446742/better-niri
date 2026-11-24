use std::future::pending;

fn main() -> anyhow::Result<()> {
    async_io::block_on(run())
}

async fn run() -> anyhow::Result<()> {
    let _connection = better_portal::serve().await?;
    pending::<()>().await;
    Ok(())
}
