use serenity::builder::{CreateCommand, CreateCommandOption};
use serenity::model::application::{CommandOptionType, ResolvedOption, ResolvedValue};

use crate::{IBundleClient, MediaBundle};

pub enum GrantAccessError {
    EmailMalFormed,
    RequestFailed(String),
}

impl std::fmt::Display for GrantAccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GrantAccessError")
    }
}

pub async fn run<C: IBundleClient>(
    options: &[ResolvedOption<'_>],
    media_bundle: &MediaBundle<C>,
) -> Result<String, GrantAccessError> {
    if let Some(ResolvedOption {
        value: ResolvedValue::String(email),
        ..
    }) = options.first()
    {
        let res = media_bundle.grant_library_access(email).await;
        match res {
            Ok(reqwest::StatusCode::OK) => Ok("Success".to_string()),
            Ok(_) => Err(GrantAccessError::RequestFailed(
                "Unexpected status code".to_string(),
            )),
            _ => Err(GrantAccessError::RequestFailed(
                "Request failed".to_string(),
            )),
        }
    } else {
        Err(GrantAccessError::EmailMalFormed)
    }
}

pub fn register() -> CreateCommand {
    CreateCommand::new("grant_access")
        .description(
            "Grant user access to libraries access.\nThis command grants access to both libraries.",
        )
        .add_option(
            CreateCommandOption::new(
                CommandOptionType::String,
                "email",
                "The email to associated with the user to which access will be granted",
            )
            .required(true),
        )
}
