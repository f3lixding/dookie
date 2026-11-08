use crate::media_bundle::IBundleClient;
use async_trait::async_trait;
use std::error::Error;

/// This is really just a wrapper around reqwest::Client.
/// The existence of it is to also retain information about the port. This way the constructor for
/// ServerEntity is "prettier".
#[derive(Debug, Clone, Default)]
pub struct BundleClient {
    client: reqwest::Client,
    token_name: String,
    token_value: String,
    cookie: String,
    port: u16,
}

#[async_trait]
impl IBundleClient for BundleClient {
    fn from_port(port: u16) -> Self {
        BundleClient {
            client: reqwest::Client::new(),
            token_name: String::default(),
            token_value: String::default(),
            cookie: String::default(),
            port,
        }
    }

    fn set_port(&mut self, port: u16) {
        self.port = port;
    }

    fn set_token(&mut self, token: (impl AsRef<str>, impl AsRef<str>)) {
        self.token_name = token.0.as_ref().to_string();
        self.token_value = token.1.as_ref().to_string();
    }

    fn set_cookie(&mut self, cookie: impl AsRef<str>) {
        self.cookie = cookie.as_ref().to_string();
    }

    #[allow(refining_impl_trait)]
    async fn get(&self, url: &str) -> Result<reqwest::Response, Box<dyn Error + Send + Sync>> {
        // Eventually we are going to have to support adding headers but for now let's just leave
        // it with the standardard headers
        let url = format!("http://localhost:{}{}", self.port, url);
        let mut request = self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json");
        if !self.token_name.is_empty() {
            request = request.header(&self.token_name, &self.token_value);
        }
        Ok(if self.cookie.is_empty() {
            request.send().await?
        } else {
            request.header("Cookie", &self.cookie).send().await?
        })
    }

    #[allow(refining_impl_trait)]
    async fn post(
        &self,
        url: &str,
        body: impl Into<reqwest::Body> + Send + Sync,
    ) -> Result<reqwest::Response, Box<dyn Error + Send + Sync>> {
        let url = format!("http://localhost:{}{}", self.port, url);
        let msg_body: reqwest::Body = body.into();
        let content_type = if url.ends_with("/api/v2/torrents/delete")
            || url.ends_with("/api/v2/auth/login")
        {
            "application/x-www-form-urlencoded"
        } else {
            "application/json"
        };

        let mut request = self
            .client
            .post(&url)
            .header("Accept", "application/json")
            .header("Content-Type", content_type)
            .body::<reqwest::Body>(msg_body);
        if !self.token_name.is_empty() {
            request = request.header(&self.token_name, &self.token_value);
        }
        Ok(if self.cookie.is_empty() {
            request.send().await?
        } else {
            request.header("Cookie", &self.cookie).send().await?
        })
    }

    #[allow(refining_impl_trait)]
    async fn delete(&self, url: &str) -> Result<reqwest::Response, Box<dyn Error + Send + Sync>> {
        let url = format!("http://localhost:{}{}", self.port, url);
        let mut request = self
            .client
            .delete(&url)
            .header("Accept", "application/json");
        if !self.token_name.is_empty() {
            request = request.header(&self.token_name, &self.token_value);
        }
        Ok(request.send().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{http::HeaderMap, routing::any, Json, Router};

    #[tokio::test]
    async fn requests_support_optional_token_headers() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().fallback(any(|headers: HeaderMap| async move {
                    Json(serde_json::json!({
                        "token": headers.get("x-api-key").map(|value| value.to_str().unwrap()),
                        "cookie": headers.get("cookie").map(|value| value.to_str().unwrap()),
                    }))
                })),
            )
            .await
            .unwrap();
        });

        // A named header with an empty value is still valid and must not be dropped.
        for token in [None, Some(""), Some("test-token")] {
            let mut client = BundleClient::from_port(port);
            if let Some(token) = token {
                client.set_token(("X-Api-Key", token));
            }
            let login = client
                .post("/api/v2/auth/login", "username=test&password=test")
                .await;
            let login: serde_json::Value = serde_json::from_slice(
                &login
                    .unwrap()
                    .error_for_status()
                    .unwrap()
                    .bytes()
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(login["token"], serde_json::json!(token));
            assert!(login["cookie"].is_null());

            client.set_cookie("SID=test-session");
            for (index, response) in [
                client.get("/api/v2/torrents/info?filter=active").await,
                client
                    .post("/api/v2/torrents/delete", "hashes=test&deleteFiles=true")
                    .await,
                client.delete("/api/v3/movie/12").await,
            ]
            .into_iter()
            .enumerate()
            {
                let body: serde_json::Value = serde_json::from_slice(
                    &response
                        .unwrap()
                        .error_for_status()
                        .unwrap()
                        .bytes()
                        .await
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(body["token"], serde_json::json!(token));
                if index < 2 {
                    assert_eq!(body["cookie"], "SID=test-session");
                }
            }
        }
        server.abort();
    }
}
