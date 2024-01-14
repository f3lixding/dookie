use crate::media_bundle::{bundle_util, BundleClient, ServerEntity};
use async_trait::async_trait;
use std::error::Error;

const APP_NAME: &'static str = "Radarr";

pub(in crate::media_bundle) struct Radarr {
    client: BundleClient,
}

#[async_trait]
impl ServerEntity for Radarr {
    fn from_bundle_client(client: BundleClient) -> impl ServerEntity {
        Radarr { client }
    }

    fn get_app_name(&self) -> &str {
        APP_NAME
    }
}

impl Radarr {
    pub async fn find(&self, name: &str) {}
    pub async fn monitor(&self, body: &str) {}
}
