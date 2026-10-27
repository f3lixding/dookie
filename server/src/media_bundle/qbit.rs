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
        match input {
            QbitInput::ListActive => {
                let (status, body) = {
                    let response = self.client.get("/api/v2/torrents/info?filter=active").await?;
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
                    let response = self
                        .client
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
        Qbit { client }
    }

    fn get_app_name(&self) -> &str {
        "qBittorrent"
    }
}
