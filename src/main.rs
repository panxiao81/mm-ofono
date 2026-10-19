use mm_ofono::{MM_PATH, MM_SERVICE, manager::Manager};
use zbus::{connection, fdo::ObjectManager};

#[tokio::main(flavor = "current_thread")]
async fn main() -> zbus::Result<()> {
    tracing_subscriber::fmt::init();

    // Claim the name right away like real ModemManager; modems show up later
    // through ObjectManager.InterfacesAdded.
    let _conn = connection::Builder::system()?
        .serve_at(MM_PATH, ObjectManager)?
        .serve_at(MM_PATH, Manager)?
        .name(MM_SERVICE)?
        .build()
        .await?;
    tracing::info!("serving {MM_SERVICE}");

    std::future::pending().await
}
