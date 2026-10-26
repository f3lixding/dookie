use serenity::all::CreateCommand;

pub(crate) mod grant_access;
pub(crate) mod media;
pub(crate) mod system;

pub fn get_create_commands() -> Vec<CreateCommand> {
    let mut commands = vec![grant_access::register()];
    commands.extend(media::register());
    commands.extend(system::register());
    commands.push(CreateCommand::new("help").description("Show available bot commands"));
    commands
}

pub fn help(is_admin: bool) -> String {
    let mut lines = vec!["Available commands:".to_string()];
    for command in get_create_commands() {
        // Use the registration itself so help stays in sync with names, options and descriptions.
        let command = serde_json::to_value(command).unwrap();
        let name = command["name"].as_str().unwrap();
        if system::is_admin_command(name) && !is_admin {
            continue;
        }
        let mut usage = format!("/{name}");
        if let Some(options) = command["options"].as_array() {
            for option in options {
                usage.push_str(&format!(" {}:<value>", option["name"].as_str().unwrap()));
            }
        }
        lines.push(format!(
            "`{usage}` — {}{}",
            command["description"].as_str().unwrap(),
            if system::is_admin_command(name) {
                " (admin only)"
            } else {
                ""
            },
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_lists_registered_commands_for_the_callers_permissions() {
        let member = help(false);
        let admin = help(true);
        assert!(member.contains("/search_movie query:<value>"));
        assert!(member.contains("/help"));
        assert!(!member.contains("/restart_services"));
        for command in get_create_commands() {
            let command = serde_json::to_value(command).unwrap();
            let name = command["name"].as_str().unwrap();
            assert!(admin.contains(&format!("/{name}")));
            if system::is_admin_command(name) {
                assert_eq!(command["default_member_permissions"], "8");
                assert_eq!(command["dm_permission"], false);
            }
        }
    }
}
