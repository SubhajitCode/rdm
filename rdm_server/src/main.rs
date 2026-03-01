use std::io::Write;
use clap::Parser;
use rdm_server::server::AppState;

const CRATE_DIR: &str = env!("CARGO_MANIFEST_DIR");

fn workspace_root() -> &'static str {
    match CRATE_DIR.rfind('/') {
        Some(i) => &CRATE_DIR[..i],
        None    => CRATE_DIR,
    }
}

/// Emit an OSC 8 terminal hyperlink so the terminal makes `display` clickable.
/// URI format: file:///abs/path:line  (understood by iTerm2, WezTerm, Ghostty, etc.)
/// ESC ] 8 ;; <url> BEL  <display>  ESC ] 8 ;; BEL  — BEL (0x07) as terminator.
fn osc8_link(abs_path: &str, line: u32) -> String {
    let url = format!("file://{}:{}", abs_path, line);
    format!("\x1b]8;;{}\x07{}:{}\x1b]8;;\x07", url, abs_path, line)
}

#[derive(Parser)]
#[command(name = "rdmd", about = "Rust Download Manager")]
struct Args{
    #[arg(long)]
    host: Option<String>,
    #[arg(short, long)]
    port: Option<String>,
    #[arg(short, long)]
    connections: Option<usize>,
}

#[tokio::main]
async fn main() {
    let workspace = workspace_root();
    let args = Args::parse();

    let mut builder = env_logger::Builder::from_default_env();
    builder.write_style(env_logger::WriteStyle::Always);
    builder.format(move |buf, record| {
        let style     = buf.default_level_style(record.level());
        let level_str = format!("{:>5}", record.level());

        let location = match (record.file(), record.line()) {
            (Some(file), Some(line)) => {
                let abs = if file.starts_with('/') {
                    file.to_string()
                } else {
                    format!("{}/{}", workspace, file)
                };
                format!("  \x1b[2m{}\x1b[0m", osc8_link(&abs, line))
            }
            _ => String::new(),
        };

        writeln!(
            buf,
            "{}{}{} {}{}",
            style.render(),
            level_str,
            style.render_reset(),
            record.args(),
            location,
        )
    });
    builder.init();

    let host = args.host.unwrap_or(std::env::var("RDM_HOST").unwrap_or("127.0.0.1".to_string())) ;
    let port = args.port.unwrap_or(std::env::var("RDM_PORT").unwrap_or("8597".to_string()));
    let connections = args.connections.unwrap_or(std::env::var("RDM_CONN_SIZE").unwrap_or("8".to_string()).parse().unwrap());
    let addr = format!("{}:{}", host, port);

    let state = AppState::with_connections(connections);
    let app = rdm_server::server::router(state);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind address");

    log::info!("rdmd listening on http://{}  (set RDM_PORT to override)", addr);
    axum::serve(listener, app)
        .await
        .expect("server error");
}
