use std::panic;

use serdes_ai_cli::args::Cli;
use tokio::signal;
use tracing::error;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    install_panic_hook();

    // Parse CLI arguments
    let cli = Cli::parse_args();

    // Run the main application
    let mut app_task = tokio::spawn(async move { serdes_ai_cli::run(cli).await });

    let result = tokio::select! {
        join_result = &mut app_task => {
            match join_result {
                Ok(output) => output,
                Err(join_err) if join_err.is_panic() => {
                    let panic_payload = join_err.into_panic();
                    let panic_msg = panic_payload_to_string(panic_payload);
                    eprintln!("Unexpected panic: {panic_msg}");
                    eprintln!("Please file an issue with steps to reproduce.");
                    std::process::exit(1);
                }
                Err(join_err) => {
                    Err(anyhow::anyhow!("application task failed to join: {join_err}"))
                }
            }
        }
        signal_result = signal::ctrl_c() => {
            match signal_result {
                Ok(()) => {
                    eprintln!("\n Received Ctrl+C, shutting down gracefully...");
                }
                Err(err) => {
                    eprintln!("\n Failed to listen for Ctrl+C: {err}");
                }
            }
            app_task.abort();
            std::process::exit(1);
        }
    };

    match result {
        Ok(()) => {
            std::process::exit(0);
        }
        Err(err) => {
            error!(error = %err, "application exited with error");
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
    }
}

fn init_tracing() {
    // Quiet by default, and never on stdout. The default subscriber writes to
    // stdout at info level, which put log lines straight through the rendered
    // interface — boxes and answers interleaved with startup chatter. Warnings
    // and errors still surface, and RUST_LOG turns detail back on when wanted.
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_writer(std::io::stderr),
        )
        .init();
}

fn install_panic_hook() {
    panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|loc| format!("{}:{}", loc.file(), loc.line()))
            .unwrap_or_else(|| "unknown location".to_string());

        let message = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "non-string panic payload".to_string()
        };

        error!(%location, %message, "application panicked");
        eprintln!("Whoops, the app panicked at {location}: {message}");
    }));
}

fn panic_payload_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}
