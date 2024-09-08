use serenity::all::CreateCommand;

pub(crate) mod grant_access;

pub fn get_create_commands() -> Vec<CreateCommand> {
    vec![grant_access::register()]
}
