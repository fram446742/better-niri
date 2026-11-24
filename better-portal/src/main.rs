use std::future::pending;

use better_portal::{BUS_NAME, OBJECT_PATH, ScreenCastBackend, ScreenshotBackend};

fn main() -> anyhow::Result<()> {
    async_io::block_on(run())
}

async fn run() -> anyhow::Result<()> {
    let connection = zbus::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, ScreenshotBackend)?
        .serve_at(OBJECT_PATH, ScreenCastBackend::default())?
        .build()
        .await?;

    eprintln!("better-portal listening on {BUS_NAME} at {OBJECT_PATH}");
    let _ = connection;
    pending::<()>().await;
    Ok(())
}
