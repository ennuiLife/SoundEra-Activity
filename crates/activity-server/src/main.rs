use activity_core::SessionKey;
use activity_server::{config::Config, db, router, AppState};
use std::{net::SocketAddr, os::unix::fs::PermissionsExt};

fn load_or_create_key(dir: &std::path::Path) -> std::io::Result<SessionKey> {
    let path = dir.join("session.key");
    if let Ok(bytes) = std::fs::read(&path) {
        if let Some(key) = SessionKey::from_bytes(bytes) {
            return Ok(key);
        }
    }
    let key = SessionKey::generate();
    std::fs::write(&path, key.as_bytes())?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(key)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())).init();
    let config = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("soundera-activity: {e}");
            std::process::exit(2);
        }
    };
    if config.navidrome_is_unprotected() {
        tracing::warn!("NAVIDROME_URL is plain http to another machine: logins travel unprotected. Use https.");
    }
    std::fs::create_dir_all(&config.data_dir).expect("data folder");
    let _ = std::fs::set_permissions(&config.data_dir, std::fs::Permissions::from_mode(0o700));
    let key = load_or_create_key(&config.data_dir).expect("session key");
    let conn = db::open(&config.data_dir.join("activity.db")).expect("database");
    let bind = config.bind.clone();
    let app = router(AppState::new(config, key, conn));
    let listener = tokio::net::TcpListener::bind(&bind).await.expect("bind");
    tracing::info!("listening on {bind}");
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async { let _ = tokio::signal::ctrl_c().await; })
        .await
        .expect("serve");
}
