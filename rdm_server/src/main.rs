use std::io::Write;
use rdm_server::server::AppState;

/// Directory that contains this crate's Cargo.toml, embedded at compile time.
const CRATE_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// Workspace root is one directory above the crate directory.
fn workspace_root() -> &'static str {
    match CRATE_DIR.rfind('/') {
        Some(i) => &CRATE_DIR[..i],
        None    => CRATE_DIR,
    }
}

/// Emit an OSC 8 terminal hyperlink so the terminal makes `display` clickable.
/// URI format: file:///abs/path:line  (understood by iTerm2, WezTerm, Ghostty, etc.)
fn osc8_link(abs_path: &str, line: u32) -> String {
    let url = format!("file://{}:{}", abs_path, line);
    // ESC ] 8 ;; <url> BEL  <display>  ESC ] 8 ;; BEL
    // Using BEL (0x07) as the string terminator — universally supported.
    format!("\x1b]8;;{}\x07{}:{}\x1b]8;;\x07", url, abs_path, line)
}

#[tokio::main]
async fn main() {
    let workspace = workspace_root();

    let mut builder = env_logger::Builder::from_default_env();
    // Force ANSI output even when stderr is not a TTY (e.g. redirected to a file
    // or piped through a pager) so the hyperlinks are always present.
    builder.write_style(env_logger::WriteStyle::Always);
    builder.format(move |buf, record| {
        // Coloured level badge.
        let style     = buf.default_level_style(record.level());
        let level_str = format!("{:>5}", record.level());

        // Resolve the source file path to absolute so the link works from any cwd.
        let location = match (record.file(), record.line()) {
            (Some(file), Some(line)) => {
                let abs = if file.starts_with('/') {
                    file.to_string()
                } else {
                    format!("{}/{}", workspace, file)
                };
                // Dim SGR so the location is visually secondary to the message.
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
