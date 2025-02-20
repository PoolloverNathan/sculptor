use anyhow::Result;
use axum::{
    middleware::from_extractor, routing::{delete, get, post, put}, Router
};
use chrono::prelude::*;
use dashmap::DashMap;
use fern::colors::{Color, ColoredLevelConfig};
use log::info;
use uuid::Uuid;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tower_http::trace::TraceLayer;

// WebSocket worker
mod ws;
use ws::handler;

// API: Auth
mod auth;
use auth as api_auth;

// API: Server info
mod info;
use info as api_info;

// API: Profile
mod profile;
use profile as api_profile;

// Utils
mod utils;

// Config
mod config;

#[derive(Debug, Clone)]
pub struct Userinfo {
    username: String,
    uuid: Uuid,
    auth_system: api_auth::AuthSystem,

}

#[derive(Debug, Clone)]
pub struct AppState {
    // Users with incomplete authentication
    pending: Arc<Mutex<DashMap<String, String>>>, // <SHA1 serverId, USERNAME>
    // Authenticated users
    authenticated: Arc<Mutex<DashMap<String, Userinfo>>>, // <SHA1 serverId, Userinfo> NOTE: In the future, try it in a separate LockRw branch
    // Ping broadcasts for WebSocket connections
    broadcasts: Arc<Mutex<DashMap<Uuid, broadcast::Sender<Vec<u8>>>>>,
    // Advanced configured users
    advanced_users: Arc<Mutex<toml::Table>>,
}

#[tokio::main]
async fn main() -> Result<()> {
    println!("The Sculptor");
    let colors = ColoredLevelConfig::new()
        .info(Color::Green)
        .debug(Color::Magenta)
        .trace(Color::Cyan)
        .warn(Color::Yellow);
    fern::Dispatch::new()
        .format(move |out, message, record| {
            out.finish(format_args!(
                "[{} {} {}] {}",
                Local::now().to_rfc3339_opts(SecondsFormat::Millis, true),
                colors.color(record.level()),
                record.target(),
                message
            ))
        })
        .level(log::LevelFilter::Info)
        // .level_for("hyper", log::LevelFilter::Info)
        .chain(std::io::stdout())
        .chain(fern::log_file("output.log")?)
        .apply()?;

    // Config
    let config = config::Config::parse("Config.toml".into());
    let listen = config.listen.as_str();

    // State
    let state = AppState {
        pending: Arc::new(Mutex::new(DashMap::new())),
        authenticated: Arc::new(Mutex::new(DashMap::new())),
        broadcasts: Arc::new(Mutex::new(DashMap::new())),
        advanced_users: Arc::new(Mutex::new(config.advanced_users)),
    };
    
    // Automatic update of advanced_users while the server is running
    let advanced_users = state.advanced_users.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;

            let new_config = config::Config::parse("Config.toml".into()).advanced_users;
            let mut config = advanced_users.lock().await;

            if new_config != *config {
                *config = new_config;
            }
        }
    });

    let api = Router::new()
        .nest(
            "//auth",
            api_auth::router()
        )
        .route(
            "/limits",
            get(api_info::limits)
        ) // TODO:
        .route(
            "/version",
            get(api_info::version),
        )
        .route(
            "/motd",
            get(|| async { config.motd }),
        )
        .route(
            "/equip",
            post(api_profile::equip_avatar)
        )
        .route(
            "/:uuid",
            get(api_profile::user_info),
        )
        .route(
            "/:uuid/avatar",
            get(api_profile::download_avatar),
        )
        .route(
            "/avatar",
            put(api_profile::upload_avatar),
        )
        .route(
            "/avatar",
            delete(api_profile::delete_avatar),
        ); // delete Avatar

    let app = Router::new()
        .nest("/api", api)
        .route("/api/", get(api_auth::status))
        .route("/ws", get(handler))
        .route_layer(from_extractor::<api_auth::Token>())
        .with_state(state)
        .layer(TraceLayer::new_for_http().on_request(()));

    let listener = tokio::net::TcpListener::bind(listen).await?;
    info!("Listening on {}", listener.local_addr()?);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    info!("Serve stopped. Closing...");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    info!("Terminate signal received");
}
