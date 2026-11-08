//! Adapter: `idlemax doctor` gathers local facts and turns them into domain checks.
//! Every check reports instead of aborting, so one broken file never hides the rest.

mod ai;
mod checks;
mod facts;
mod host;
mod net;
mod service;

#[cfg(test)]
mod tests;

use idlemax_core::doctor::Report;
use idlemax_core::VERSION;

/// Runs every check. `coord_url` is the `--coord` override; None uses the config.
pub async fn run(coord_url: Option<String>) -> Report {
    let mut report = Report::new(VERSION, None);
    let paths = checks::home(&mut report);
    let config = checks::config(&mut report, paths.as_ref());
    let token = checks::token(&mut report, paths.as_ref());
    net::coordinator(&mut report, &config, token.as_deref(), coord_url.as_deref()).await;
    net::local_agent(&mut report, &config, token.as_deref(), paths.as_ref()).await;
    host::allowlist(&mut report, &config);
    host::disk(&mut report, &config).await;
    ai::accounts(&mut report, &config, paths.as_ref()).await;
    host::github(&mut report, &config).await;
    service::check(&mut report, facts::home_dir().as_deref());
    report
}
