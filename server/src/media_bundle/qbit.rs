use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::error::Error;

use crate::media_bundle::{BundleResponse, IBundleClient, ServerEntity};

pub(in crate::media_bundle) enum QbitInput {}
pub(in crate::media_bundle) enum QbitOutput {}

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
        unimplemented!()
    }

    fn from_bundle_client(client: C) -> Self {
        Qbit { client }
    }

    fn get_app_name(&self) -> &str {
        unimplemented!()
    }
}
