use crate::media_bundle::{IBundleClient, ServerEntity};
use async_trait::async_trait;

const APP_NAME: &'static str = "Radarr";

pub(in crate::media_bundle) struct Radarr<C>
where
    C: IBundleClient,
{
    client: C,
}

#[async_trait]
impl<C> ServerEntity for Radarr<C>
where
    C: IBundleClient,
{
    type Input = ();
    type Output = ();
    type Client = C;

    async fn make_call(&self, input: Self::Input) -> Self::Output {
        todo!()
    }

    fn from_bundle_client(client: Self::Client) -> impl ServerEntity {
        Radarr { client }
    }

    fn get_app_name(&self) -> &str {
        APP_NAME
    }
}

impl<C> Radarr<C>
where
    C: IBundleClient,
{
    pub async fn find(&self, name: &str) {}
    pub async fn monitor(&self, body: &str) {}
}
