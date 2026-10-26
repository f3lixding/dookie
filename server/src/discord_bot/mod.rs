mod commands;
mod discord_bot;

use commands::grant_access::GrantAccessError;
use commands::media::MediaCommandError;
pub use discord_bot::*;

#[derive(Debug)]
pub enum CommandError {
    GrantAccessError(GrantAccessError),
    MediaCommandError(MediaCommandError),
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandError::GrantAccessError(err) => err.fmt(f),
            CommandError::MediaCommandError(err) => err.fmt(f),
        }
    }
}
