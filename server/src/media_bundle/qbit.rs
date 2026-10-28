use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::error::Error;

use crate::media_bundle::{BundleResponse, IBundleClient, ServerEntity};

pub(in crate::media_bundle) enum QbitInput {
    ListActive,
    Delete(String),
}

pub(in crate::media_bundle) enum QbitOutput {
    Torrents(Vec<TorrentInfo>),
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TorrentInfo {
    pub hash: String,
    pub name: String,
    pub progress: f64,
    pub dlspeed: u64,
    pub upspeed: u64,
}

#[derive(Default, Debug, Clone)]
pub(in crate::media_bundle) struct Qbit<C>
where
    C: IBundleClient,
{
    client: C,
    username: String,
    password: String,
}

impl<C> Qbit<C>
where
    C: IBundleClient,
{
    pub fn new(client: C, username: String, password: String) -> Self {
        Self {
            client,
            username,
            password,
        }
    }
}

#[async_trait]
impl<C> ServerEntity for Qbit<C>
where
    C: IBundleClient,
{
    type Input = QbitInput;
    type Output = QbitOutput;
    type Client = C;

    async fn make_call(
        &self,
        input: Self::Input,
    ) -> Result<Self::Output, Box<dyn Error + Send + Sync>> {
        let mut client = self.client.clone();
        let login_body = format!(
            "username={}&password={}",
            urlencoded(&self.username),
            urlencoded(&self.password)
        );
        let (status, cookie, body) = {
            let response = client.post("/api/v2/auth/login", login_body).await?;
            (
                response.get_statuscode(),
                response.get_cookie(),
                response.as_bytes(),
            )
        };
        let body = body.await?;
        if !(200..300).contains(&status) || cookie.is_none() {
            return Err(format!(
                "qBittorrent login failed (HTTP {status}): {}",
                String::from_utf8_lossy(&body)
            )
            .into());
        }
        client.set_cookie(cookie.expect("checked above"));

        match input {
            QbitInput::ListActive => {
                let (status, body) = {
                    let response = client.get("/api/v2/torrents/info?filter=active").await?;
                    (response.get_statuscode(), response.as_bytes())
                };
                let body = body.await?;
                if !(200..300).contains(&status) {
                    return Err(format!("qBittorrent returned HTTP {status}").into());
                }
                Ok(QbitOutput::Torrents(serde_json::from_slice(&body)?))
            }
            QbitInput::Delete(hash) => {
                let (status, body) = {
                    let response = client
                        .post(
                            "/api/v2/torrents/delete",
                            format!("hashes={hash}&deleteFiles=true"),
                        )
                        .await?;
                    (response.get_statuscode(), response.as_bytes())
                };
                let body = body.await?;
                if !(200..300).contains(&status) {
                    return Err(format!(
                        "qBittorrent returned HTTP {status}: {}",
                        String::from_utf8_lossy(&body)
                    )
                    .into());
                }
                Ok(QbitOutput::Deleted)
            }
        }
    }

    fn from_bundle_client(client: C) -> Self {
        Qbit::new(client, String::new(), String::new())
    }

    fn get_app_name(&self) -> &str {
        "qBittorrent"
    }
}

fn urlencoded(value: &str) -> String {
    value.bytes().fold(String::new(), |mut encoded, byte| {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
        encoded
    })
}
