//! Adapter: `idlemax doctor` gathers local facts and turns them into domain checks.
//! Every check reports instead of aborting, so one broken file never hides the rest.

mod ai;
mod checks;
pub(crate) mod facts;
mod host;
mod logins;
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
    let units = service::installed(facts::home_dir().as_deref());
    let agent_unit = service::agent_installed(&units);
    net::local_agent(
        &mut report,
        &config,
        token.as_deref(),
        paths.as_ref(),
        agent_unit,
    )
    .await;
    host::allowlist(&mut report, &config, service::agent_path(&units).as_deref());
    host::disk(&mut report, &config, paths.as_ref()).await;
    ai::accounts(&mut report, &config, paths.as_ref()).await;
    logins::check(&mut report, &config).await;
    host::sandbox(&mut report, &config).await;
    host::github(&mut report, &config).await;
    service::check(&mut report, &units);
    skill(&mut report);
    report
}

/// Each coding agent here has the current idlemax skill, so it can bring idlemax's work back
/// to the user; nothing to check without a home to find the agents in.
fn skill(report: &mut Report) {
    let Ok(places) = crate::cli::hooks::places::Places::from_env() else {
        return;
    };
    let generated = crate::cli::skill::generate();
    match crate::cli::skill::install::copies(&places, &generated) {
        Ok(copies) => report.push(idlemax_core::install::skill::check(&copies)),
        Err(e) => report.push(idlemax_core::doctor::Check::warn(
            "skill",
            format!("could not read the agents' skill copies: {e:#}"),
            "idlemax skill install",
        )),
    }
}
