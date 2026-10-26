use serenity::all::{
    CommandOptionType, CreateAttachment, CreateCommand, CreateCommandOption,
    EditInteractionResponse, Permissions, ResolvedOption, ResolvedValue,
};
use std::{process::Output, time::Duration};
use tokio::{process::Command, time};

pub fn is_admin_command(name: &str) -> bool {
    matches!(name, "docker_ps" | "restart_container" | "restart_services")
}

pub fn register() -> Vec<CreateCommand> {
    vec![
        CreateCommand::new("docker_ps").description("List running Docker containers"),
        CreateCommand::new("restart_container")
            .description("Stop and restart a Docker container")
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "container_id",
                    "Container ID or name from /docker_ps",
                )
                .required(true),
            ),
        CreateCommand::new("restart_services")
            .description("Shut down and reopen Plex, Sonarr, Radarr, and Prowlarr on macOS"),
    ]
    .into_iter()
    .map(|command| {
        command
            .default_member_permissions(Permissions::ADMINISTRATOR)
            .dm_permission(false)
    })
    .collect()
}

async fn execute(program: &str, args: &[&str]) -> Result<Output, String> {
    time::timeout(
        Duration::from_secs(90),
        Command::new(program).args(args).kill_on_drop(true).output(),
    )
    .await
    .map_err(|_| format!("{program} timed out"))?
    .map_err(|error| format!("Cannot run {program}: {error}"))
}

fn checked_output(program: &str, output: Output) -> Result<String, String> {
    if !output.status.success() {
        return Err(format!(
            "{program} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn valid_container(container: &str) -> bool {
    // No shell is used, but reject flags and multiple targets as well.
    !container.is_empty()
        && container.starts_with(|c: char| c.is_ascii_alphanumeric())
        && container
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

pub async fn run(
    name: &str,
    options: &[ResolvedOption<'_>],
) -> Result<EditInteractionResponse, String> {
    match name {
        "docker_ps" => {
            let output = checked_output("docker", execute("docker", &["container", "ls"]).await?)?;
            if output.encode_utf16().count() <= 1700 && !output.contains("```") {
                Ok(EditInteractionResponse::new().content(format!(
                    "Running Docker containers:\n```text\n{output}\n```"
                )))
            } else {
                Ok(EditInteractionResponse::new()
                    .content("Running Docker containers (full listing attached):")
                    .new_attachment(CreateAttachment::bytes(
                        output.into_bytes(),
                        "containers.txt",
                    )))
            }
        }
        "restart_container" => {
            let container = options
                .iter()
                .find_map(|option| match option.value {
                    ResolvedValue::String(value) if option.name == "container_id" => Some(value),
                    _ => None,
                })
                .filter(|value| valid_container(value))
                .ok_or("Provide one valid container ID or name (not a Docker flag).")?;
            checked_output(
                "docker",
                execute("docker", &["container", "restart", container]).await?,
            )?;
            Ok(EditInteractionResponse::new()
                .content(format!("Restarted container `{container}`.")))
        }
        "restart_services" => {
            if !cfg!(target_os = "macos") {
                return Err("Service restarts require macOS and /Applications app bundles.".into());
            }
            let mut results = Vec::new();
            for app in ["Plex Media Server", "Sonarr", "Radarr", "Prowlarr"] {
                results.push(match restart_app(app).await {
                    Ok(()) => format!("{app}: shutdown complete; app reopened."),
                    Err(error) => format!("{app}: {}", error.chars().take(300).collect::<String>()),
                });
            }
            Ok(EditInteractionResponse::new().content(results.join("\n")))
        }
        _ => Err("Unknown system command".into()),
    }
}

async fn restart_app(app: &str) -> Result<(), String> {
    let path = format!("/Applications/{app}.app");
    // Check before stopping anything, so a missing bundle does not leave a service down.
    if !tokio::fs::try_exists(&path)
        .await
        .map_err(|error| error.to_string())?
    {
        return Err(format!("App bundle not found: {path}"));
    }
    // Match the executable inside this app bundle, not a truncated process name or a shell
    // whose arguments happen to mention the app. In particular, Plex's name exceeds 15 chars.
    let pattern = format!("^/Applications/{app}\\.app/Contents/MacOS/{app}([[:space:]]|$)");
    let stopped = execute("pkill", &["-TERM", "-f", &pattern]).await?;
    // pkill exits 1 when the app was already stopped.
    if !stopped.status.success() && stopped.status.code() != Some(1) {
        checked_output("pkill", stopped)?;
    }
    let wait = async {
        loop {
            let running = execute("pgrep", &["-f", &pattern]).await?;
            if running.status.code() == Some(1) {
                return Ok::<(), String>(());
            }
            checked_output("pgrep", running)?;
            time::sleep(Duration::from_millis(250)).await;
        }
    };
    time::timeout(Duration::from_secs(30), wait)
        .await
        .map_err(|_| {
            "App did not shut down within 30 seconds; not opening another instance.".to_string()
        })??;
    checked_output("open", execute("open", &[&path]).await?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn container_restart_rejects_flags_and_multiple_targets() {
        for value in ["", "--all", "-t", "qbit other", "qbit; reboot", "$(reboot)"] {
            let data: serenity::all::CommandData = serde_json::from_value(serde_json::json!({
                "id": "1", "name": "restart_container", "type": 1,
                "options": [{ "name": "container_id", "type": 3, "value": value }],
            }))
            .unwrap();
            let error = run("restart_container", &data.options()).await.unwrap_err();
            assert!(error.contains("valid container ID"), "{value}: {error}");
        }
    }
}
