use crate::media_bundle::{bundle_util, BundleClient, ServerEntity};
use async_trait::async_trait;
use std::error::Error;

pub(in crate::media_bundle) struct Radarr {
    client: BundleClient,
}

#[async_trait]
impl ServerEntity for Radarr {
    fn from_bundle_client(client: BundleClient) -> impl ServerEntity {
        Radarr { client }
    }

    async fn start(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        Ok(())
    }
}
