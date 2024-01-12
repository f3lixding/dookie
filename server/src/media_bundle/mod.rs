mod plex;
mod prowlarr;
mod qbit;
mod radarr;
mod sonarr;

use plex::*;
use prowlarr::*;
use qbit::*;
use radarr::*;
use sonarr::*;

use async_trait::async_trait;
use std::error::Error;

#[async_trait]
pub(in crate::media_bundle) trait ServerEntity {
    async fn start(&self) -> Result<(), Box<dyn Error + Send + Sync>>;
    async fn shutdown(&self) -> Result<(), Box<dyn Error + Send + Sync>>;
}

#[derive(Debug, Clone)]
pub struct MediaBundle {}

unsafe impl Send for MediaBundle {}
unsafe impl Sync for MediaBundle {}
