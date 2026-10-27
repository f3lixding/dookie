use crate::discord_bot::CommandError;
use crate::{IBundleClient, MediaBundle, TorrentInfo};
use serenity::builder::{CreateAttachment, CreateCommand, CreateCommandOption, EditInteractionResponse};
use serenity::model::application::{CommandOptionType, ResolvedOption, ResolvedValue};
use serenity::model::permissions::Permissions;

#[derive(Debug)]
pub enum QbitCommandError {
    MissingHash,
    InvalidHash,
    RequestFailed(String),
}

impl std::fmt::Display for QbitCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingHash => write!(f, "Missing torrent hash"),
            Self::InvalidHash => write!(f, "Provide a valid 40-character torrent hash"),
            Self::RequestFailed(error) => write!(f, "Request failed: {error}"),
        }
    }
}

impl From<QbitCommandError> for CommandError {
    fn from(value: QbitCommandError) -> Self {
        CommandError::QbitCommandError(value)
    }
}

pub fn register() -> Vec<CreateCommand> {
    vec![
        CreateCommand::new("torrent_list")
            .description("List active qBittorrent downloads and uploads"),
        CreateCommand::new("torrent_delete")
            .description("Delete a qBittorrent item and its files from disk")
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "hash",
                    "The 40-character hash shown by /torrent_list",
                )
                .required(true),
            ),
    ]
    .into_iter()
    .map(|command| {
        command
            .default_member_permissions(Permissions::ADMINISTRATOR)
            .dm_permission(false)
    })
    .collect()
}

pub async fn list<C: IBundleClient>(
    media_bundle: &MediaBundle<C>,
) -> Result<EditInteractionResponse, QbitCommandError> {
    let torrents = media_bundle
        .list_active_torrents()
        .await
        .map_err(|error| QbitCommandError::RequestFailed(error.to_string()))?;
    let body = if torrents.is_empty() {
        "No active torrents.".to_string()
    } else {
        torrents.iter().map(format_torrent).collect::<Vec<_>>().join("\n")
    };

    if body.encode_utf16().count() <= 1900 && !body.contains("```") {
        Ok(EditInteractionResponse::new().content(format!("```text\n{body}\n```")))
    } else {
        Ok(EditInteractionResponse::new()
            .content("Active torrents are attached as a text file.")
            .new_attachment(CreateAttachment::bytes(body.into_bytes(), "torrents.txt")))
    }
}

pub async fn delete<C: IBundleClient>(
    options: &[ResolvedOption<'_>],
    media_bundle: &MediaBundle<C>,
) -> Result<EditInteractionResponse, QbitCommandError> {
    let hash = options
        .iter()
        .find_map(|option| match option.value {
            ResolvedValue::String(value) if option.name == "hash" => Some(value),
            _ => None,
        })
        .ok_or(QbitCommandError::MissingHash)?;
    if !valid_hash(hash) {
        return Err(QbitCommandError::InvalidHash);
    }
    media_bundle
        .delete_torrent(hash)
        .await
        .map_err(|error| QbitCommandError::RequestFailed(error.to_string()))?;
    Ok(EditInteractionResponse::new().content(format!(
        "Deleted torrent `{hash}` and its files from disk."
    )))
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 40 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn format_torrent(torrent: &TorrentInfo) -> String {
    format!(
        "{} | {:.1}% | ↓ {}/s | ↑ {}/s | {}",
        torrent.name,
        torrent.progress * 100.0,
        format_speed(torrent.dlspeed),
        format_speed(torrent.upspeed),
        torrent.hash,
    )
}

fn format_speed(bytes_per_second: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes_per_second as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes_per_second, UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_only_full_torrent_hashes() {
        assert!(valid_hash(&"a".repeat(40)));
        assert!(!valid_hash("a"));
        assert!(!valid_hash(&"g".repeat(40)));
    }
}
