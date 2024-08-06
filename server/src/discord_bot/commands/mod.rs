use serenity::all::CreateCommand;

mod grant_access;

pub fn get_create_commands() -> Vec<CreateCommand> {
    vec![grant_access::register()]
}
