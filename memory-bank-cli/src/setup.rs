mod plan;
mod prompts;
mod render;

use crate::AppError;
use crate::agents::{AgentSetupOutcome, configure_selected_agents, detect_installed_agents};
use crate::assets::{ExposureOutcome, materialize_and_expose_cli};
use crate::config::fastembed_reindex_change;
use crate::constants::{HEALTH_POLL_INTERVAL, HEALTH_STARTUP_TIMEOUT};
use crate::output::{styled_failure, styled_subtle, styled_success, styled_warning};
use crate::service::{HealthCheck, install_service, start_service, wait_for_health};
use memory_bank_app::{AppPaths, AppSettings, SecretStore, default_server_url};
use std::io::{self, Write};

use self::plan::{SetupPlan, apply_secret_choice, build_settings_for_plan};
use self::prompts::{collect_setup_plan, configure_setup_rendering, ensure_interactive_terminal};
use self::render::{render_post_setup_help, render_review_summary};

pub(crate) fn run_setup() -> Result<(), AppError> {
    ensure_interactive_terminal()?;
    configure_setup_rendering();

    let paths = AppPaths::from_system()?;
    let model_catalog = crate::models::refresh_model_catalog(&paths);
    let mut settings = AppSettings::load(&paths)?;
    let mut secrets = SecretStore::load(&paths)?;
    let detected_agents = detect_installed_agents();
    let plan = match collect_setup_plan(&settings, &secrets, &detected_agents, &model_catalog) {
        Ok(plan) => plan,
        Err(AppError::SetupCanceled) => {
            println!(
                "{}",
                styled_warning("Setup canceled. No changes were made.")
            );
            return Ok(());
        }
        Err(error) => return Err(error),
    };

    println!();
    println!("{}", render_review_summary(&plan));
    let preview_settings = build_settings_for_plan(&settings, &plan, &[]);
    if let Some(change) = fastembed_reindex_change(&settings, &preview_settings) {
        println!();
        println!(
            "{}",
            styled_warning(&format!(
                "Warning: Changing the FastEmbed model from `{}` to `{}` means the next service start will rebuild the vector index and re-encode existing memories for this namespace.",
                change.previous_model, change.new_model
            ))
        );
    }
    println!();

    let confirm = inquire::Confirm::new("Apply these changes now?")
        .with_default(true)
        .with_help_message(
            "Nothing under ~/.memory_bank or your agent config files will change until you confirm.",
        )
        .prompt_skippable()?;
    if !matches!(confirm, Some(true)) {
        println!(
            "{}",
            styled_warning("Setup canceled. No changes were made.")
        );
        return Ok(());
    }

    let (health, agent_outcome, exposure) =
        apply_setup_plan(&paths, &mut settings, &mut secrets, &plan)?;
    println!();
    println!(
        "{}",
        styled_success(&format!(
            "Memory Bank is ready on {} using namespace `{}` and provider `{}`.",
            default_server_url(&settings),
            health.namespace,
            health.llm_provider
        ))
    );
    if !agent_outcome.warnings.is_empty() {
        println!(
            "{}",
            styled_warning("Some agent integrations need attention:")
        );
        for warning in agent_outcome.warnings {
            println!("  - {warning}");
        }
    }
    println!();
    println!("{}", render_post_setup_help(&exposure));
    Ok(())
}

fn apply_setup_plan(
    paths: &AppPaths,
    settings: &mut AppSettings,
    secrets: &mut SecretStore,
    plan: &SetupPlan,
) -> Result<(HealthCheck, AgentSetupOutcome, ExposureOutcome), AppError> {
    let total_steps = 6;
    let preview_settings = build_settings_for_plan(settings, plan, &[]);

    let exposure = apply_step(1, total_steps, "Install artifacts and expose CLI", || {
        paths.ensure_base_dirs()?;
        materialize_and_expose_cli(paths)
    })?;

    let agent_outcome = {
        print_step_start(2, total_steps, "Configure selected agents")?;
        let outcome = configure_selected_agents(paths, &preview_settings, &plan.selected_agents)?;
        if plan.selected_agents.is_empty() {
            println!("{}", styled_subtle("skipped (no agents selected)"));
        } else if outcome.warnings.is_empty() {
            println!("{}", styled_success("done"));
        } else {
            println!("{}", styled_warning("done with warnings"));
        }
        outcome
    };

    *settings = build_settings_for_plan(settings, plan, &agent_outcome.configured);

    apply_step(3, total_steps, "Write settings and secrets", || {
        apply_secret_choice(secrets, &plan.secret_choice);
        settings.save(paths)?;
        secrets.save(paths)?;
        Ok(())
    })?;

    apply_step(4, total_steps, "Install managed service", || {
        install_service(paths, settings)
    })?;
    apply_step(5, total_steps, "Start managed service", || {
        start_service(paths, settings)
    })?;
    let health = apply_step(6, total_steps, "Wait for service health", || {
        wait_for_health(settings, HEALTH_STARTUP_TIMEOUT, HEALTH_POLL_INTERVAL)
    })?;

    Ok((health, agent_outcome, exposure))
}

fn print_step_start(index: usize, total: usize, label: &str) -> Result<(), AppError> {
    print!(
        "{} {label}... ",
        styled_subtle(&format!("[{index}/{total}]"))
    );
    io::stdout().flush()?;
    Ok(())
}

fn apply_step<T, F>(index: usize, total: usize, label: &str, action: F) -> Result<T, AppError>
where
    F: FnOnce() -> Result<T, AppError>,
{
    print_step_start(index, total, label)?;
    match action() {
        Ok(value) => {
            println!("{}", styled_success("done"));
            Ok(value)
        }
        Err(error) => {
            println!("{}", styled_failure("failed"));
            Err(error)
        }
    }
}
