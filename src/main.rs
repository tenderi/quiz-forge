//! quiz-forge: serve | migrate | backup | check-config. See docs/DESIGN.md.

use anyhow::Context;
use quiz_forge::agent::{ReviewProvider, anthropic::Anthropic};
use quiz_forge::config::Config;
use quiz_forge::service::telegram::FakeTelegram;
use quiz_forge::service::{App, Telegram, questions};
use std::sync::Arc;

const USAGE: &str = "usage: quiz-forge [serve | migrate | backup <dir> | check-config | --version]

Configuration comes from the environment, or a .env file in the working
directory. See .env.example.";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn,teloxide=warn,hyper=warn,reqwest=warn".into()),
        )
        .init();
    // teloxide and reqwest bring different rustls crypto backends; pick one.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str).unwrap_or("serve") {
        "serve" => serve().await,
        "migrate" => {
            let cfg = Config::from_env()?;
            open(&cfg).await?;
            println!("database at {} is up to date", cfg.db_path().display());
            Ok(())
        }
        "backup" => {
            let dir = args.get(1).context("backup needs a target directory")?;
            let cfg = Config::from_env()?;
            let pool = open(&cfg).await?;
            let out = quiz_forge::backup::run(
                &pool,
                &cfg.media_dir(),
                std::path::Path::new(dir),
                chrono::Utc::now(),
            )
            .await?;
            println!("{out}");
            Ok(())
        }
        "check-config" => {
            let cfg = Config::from_env()?;
            println!(
                "group {} · {} · data in {} · agent {}",
                cfg.group_chat_id,
                cfg.public_base_url,
                cfg.data_dir.display(),
                if cfg.agent.enabled { "on" } else { "off" }
            );
            Ok(())
        }
        "--version" | "version" => {
            println!("quiz-forge {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

async fn open(cfg: &Config) -> anyhow::Result<sqlx::SqlitePool> {
    std::fs::create_dir_all(&cfg.data_dir)
        .with_context(|| format!("creating {}", cfg.data_dir.display()))?;
    quiz_forge::db::open(&cfg.db_path()).await
}

async fn serve() -> anyhow::Result<()> {
    let cfg = Config::from_env()?;
    let pool = open(&cfg).await?;
    let bot = cfg.telegram_token.clone().map(teloxide::Bot::new);
    let tg: Arc<dyn Telegram> = match &bot {
        Some(b) => Arc::new(quiz_forge::bot::tg::TeloxideTelegram { bot: b.clone() }),
        None => {
            tracing::warn!(
                "TELEGRAM_BOT_TOKEN is not set: running without Telegram (messages are dropped)"
            );
            Arc::new(FakeTelegram::default())
        }
    };
    let provider: Option<Arc<dyn ReviewProvider>> = cfg
        .agent
        .enabled
        .then(|| Arc::new(Anthropic::new(cfg.agent.clone())) as Arc<dyn ReviewProvider>);
    let listen = cfg.listen.clone();
    let app = App::new(pool, cfg, tg);
    questions::seed_themes(&app, app.now()).await?;

    tokio::spawn(quiz_forge::scheduler::run(app.clone(), provider));
    if let Some(b) = bot {
        let app = app.clone();
        tokio::spawn(async move {
            if let Err(e) = quiz_forge::bot::tg::run(app, b).await {
                tracing::error!("the bot stopped: {e:#}");
            }
        });
    }

    let router = quiz_forge::web::router(app);
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    if let Some(path) = listen.strip_prefix("unix:") {
        let _ = std::fs::remove_file(path);
        let listener =
            tokio::net::UnixListener::bind(path).with_context(|| format!("binding {path}"))?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))?;
        }
        tracing::info!("web app on unix:{path}");
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown)
            .await?;
    } else {
        let listener = tokio::net::TcpListener::bind(&listen)
            .await
            .with_context(|| format!("binding {listen}"))?;
        tracing::info!("web app on http://{}", listener.local_addr()?);
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown)
            .await?;
    }
    Ok(())
}
