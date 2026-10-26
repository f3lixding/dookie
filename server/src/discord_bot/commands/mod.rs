use serenity::all::CreateCommand;

pub(crate) mod grant_access;
pub(crate) mod media;

pub fn get_create_commands() -> Vec<CreateCommand> {
    let mut commands = vec![grant_access::register()];
    commands.extend(media::register());
    commands
}
