//! The background loop: closes expired votes, delivers the outbox and starts
//! the review agent when it's due. Ticks every minute, and delivers at once
//! when something is queued.

use crate::agent::{self, ReviewProvider};
use crate::service::{App, outbox, questions};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const TICK: Duration = Duration::from_secs(60);

/// One tick's worth of work, minus the agent.
pub async fn tick(app: &App) -> anyhow::Result<()> {
    let now = app.now();
    let expired = questions::expire_votes(app, now).await?;
    if expired > 0 {
        tracing::info!("{expired} question(s) rejected at the end of their voting window");
    }
    outbox::deliver_due(app, now).await?;
    Ok(())
}

/// Starts an agent run in the background unless one is running or none is due.
/// Returns whether one was started.
pub async fn maybe_review(
    app: &App,
    provider: &Arc<dyn ReviewProvider>,
    running: &Arc<AtomicBool>,
) -> anyhow::Result<bool> {
    if running.load(Ordering::SeqCst) || !agent::due(app, app.now()).await? {
        return Ok(false);
    }
    agent::mark_ran(app, app.now()).await?;
    running.store(true, Ordering::SeqCst);
    let (app, provider, running) = (app.clone(), provider.clone(), running.clone());
    tokio::spawn(async move {
        match agent::run_once(&app, provider.as_ref()).await {
            Ok(s) => tracing::info!(
                "review run {}: {} reviewed, {} failed, ${:.2}{}",
                s.run_id,
                s.reviewed,
                s.failed,
                s.cost_usd,
                if s.stopped_on_budget {
                    " (stopped at the cost cap)"
                } else {
                    ""
                }
            ),
            Err(e) => tracing::error!("review run failed: {e:#}"),
        }
        running.store(false, Ordering::SeqCst);
        app.outbox_wake.notify_one();
    });
    Ok(true)
}

pub async fn run(app: App, provider: Option<Arc<dyn ReviewProvider>>) {
    let running = Arc::new(AtomicBool::new(false));
    let mut interval = tokio::time::interval(TICK);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Err(e) = tick(&app).await {
                    tracing::error!("scheduler tick failed: {e:#}");
                }
                if let Some(p) = &provider
                    && let Err(e) = maybe_review(&app, p, &running).await {
                        tracing::error!("starting a review run failed: {e:#}");
                    }
            }
            _ = app.outbox_wake.notified() => {
                if let Err(e) = outbox::deliver_due(&app, app.now()).await {
                    tracing::error!("outbox delivery failed: {e:#}");
                }
            }
        }
    }
}
