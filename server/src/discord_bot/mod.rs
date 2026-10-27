mod commands;
mod discord_bot;

use commands::grant_access::GrantAccessError;
use commands::media::MediaCommandError;
use commands::qbit::QbitCommandError;
pub use discord_bot::*;

#[derive(Debug)]
pub enum CommandError {
    GrantAccessError(GrantAccessError),
    MediaCommandError(MediaCommandError),
    QbitCommandError(QbitCommandError),
    SystemCommandError(String),
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandError::GrantAccessError(err) => err.fmt(f),
            CommandError::MediaCommandError(err) => err.fmt(f),
            CommandError::QbitCommandError(err) => err.fmt(f),
            CommandError::SystemCommandError(err) => err.fmt(f),
        }
    }
}
