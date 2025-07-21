use axum::routing::get;
use tower_sessions::{Expiry, MemoryStore, SessionManagerLayer};
use tracing::info;
use tracing_subscriber::prelude::*;
use trainee_tracker::{Config, ServerState};

use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 1 {
        panic!(
            "Expected exactly one argument (path to config file), got {}",
            args.len()
        );
    }

    let stderr_log_level = tracing_subscriber::filter::LevelFilter::INFO;
    let stderr_layer = tracing_subscriber::fmt::layer()
        .pretty()
        .with_writer(std::io::stderr);

    tracing_subscriber::registry()
        .with(stderr_layer.with_filter(stderr_log_level))
        .try_init()
        .expect("Failed to configure logging");

    let config_bytes = std::fs::read(&args[0]).expect("Failed to read config file");
    let config: Config =
        serde_json::from_slice(&config_bytes).expect("Failed to parse config file");

    let addr = config.addr.unwrap_or_else(|| "127.0.0.1".parse().unwrap());
    let sock_addr = SocketAddr::from((addr, config.port));

    let is_secure = config.public_base_url.starts_with("https://");

    let server_state = ServerState::new(config);

    let session_store = MemoryStore::default();
    let session_layer = SessionManagerLayer::new(session_store)
        .with_secure(is_secure)
        .with_expiry(Expiry::OnSessionEnd);

    let app = axum::Router::new()
        .route("/api/ok", get(trainee_tracker::endpoints::health_check))
        .route(
            "/api/whoami/github",
            get(trainee_tracker::endpoints::whoami_github),
        )
        .route("/api/courses", get(trainee_tracker::endpoints::courses))
        .route(
            "/api/courses/{course}/prs",
            get(trainee_tracker::endpoints::course_prs),
        )
        .route(
            "/api/courses/{course}/trainee-batches",
            get(trainee_tracker::endpoints::trainee_batches),
        )
        .route(
            "/api/courses/{course}/trainee-batches/{batch}",
            get(trainee_tracker::endpoints::trainee_batch),
        )
        .route("/api/teams", get(trainee_tracker::endpoints::teams))
        .route(
            "/api/trainees/{trainee}/region",
            get(trainee_tracker::endpoints::get_region),
        )
        .route(
            "/api/oauth-callbacks/github",
            get(trainee_tracker::auth::handle_github_oauth_callback),
        )
        .route(
            "/api/oauth-callbacks/google-drive",
            get(trainee_tracker::auth::handle_google_oauth_callback),
        )
        .route(
            "/api/oauth-callbacks/slack",
            get(trainee_tracker::auth::handle_slack_oauth_callback),
        )
        .route("/", get(trainee_tracker::frontend::index))
        .route("/courses", get(trainee_tracker::frontend::list_courses))
        .route(
            "/courses/{course}/batches/{batch_github_slug}",
            get(trainee_tracker::frontend::get_trainee_batch),
        )
        .route(
            "/courses/{course}/reviewers",
            get(trainee_tracker::frontend::get_reviewers),
        )
        .route(
            "/groups/google",
            get(trainee_tracker::frontend::list_google_groups),
        )
        .route(
            "/groups/google.csv",
            get(trainee_tracker::frontend::list_google_groups_csv),
        )
        .route(
            "/groups/slack.csv",
            get(trainee_tracker::frontend::list_slack_groups_csv),
        )
        .layer(session_layer)
        .with_state(server_state);

    let listener = tokio::net::TcpListener::bind(sock_addr)
        .await
        .expect("Failed to bind");

    info!("Listening on {:?}", sock_addr);

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .expect("Failed to serve");
}
