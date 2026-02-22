use rdm_server::server::AppState;

#[tokio::main]
async fn main() {
    env_logger::init();

    let host = std::env::var("RDM_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("RDM_PORT").unwrap_or_else(|_| "8597".to_string());
    let addr = format!("{}:{}", host, port);

    let state = AppState::new();
    let app = rdm_server::server::router(state);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind address");

    log::info!("rdmd listening on http://{}  (set RDM_PORT to override)", addr);
    axum::serve(listener, app)
        .await
        .expect("server error");
}
