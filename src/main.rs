use mm_ofono::{MM_SERVICE, run, serve};
use zbus::connection;

#[tokio::main(flavor = "current_thread")]
async fn main() -> zbus::Result<()> {
    // Plain text under systemd (the journal adds its own timestamps).
    let tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
    tracing_subscriber::fmt().with_ansi(tty).without_time().init();

    let conn = connection::Builder::system().build().await?;
    serve(&conn).await?;
    // Claim the name right away like real ModemManager; modems show up later
    // through ObjectManager.InterfacesAdded.
    conn.request_name(MM_SERVICE).await?;
    tracing::info!("serving {MM_SERVICE}");

    run(conn).await
}
