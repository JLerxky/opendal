// Licensed to the Apache Software Foundation (ASF) under one
// or more contributor license agreements.  See the NOTICE file
// distributed with this work for additional information
// regarding copyright ownership.  The ASF licenses this file
// to you under the Apache License, Version 2.0 (the
// "License"); you may not use this file except in compliance
// with the License.  You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing,
// software distributed under the License is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.  See the License for the
// specific language governing permissions and limitations
// under the License.

use std::collections::HashMap;
use std::fmt::Debug;
use std::fmt::Formatter;
use std::sync::Arc;

use chrono::DateTime;
use chrono::Utc;
use tokio::sync::Mutex;

use super::backend::DropboxBackend;
use super::core::DropboxCore;
use super::core::DropboxSigner;
use crate::raw::*;
use crate::*;

/// [Dropbox](https://www.dropbox.com/) backend support.
///
/// # Capabilities
///
/// This service can be used to:
///
/// - [x] read
/// - [x] write
/// - [x] delete
/// - [ ] copy
/// - [ ] create
/// - [ ] list
/// - [ ] rename
///
/// # Notes
///
///
/// # Configuration
///
/// - `root`: Set the work directory for this backend.
///
/// ## Credentials related
///
/// ### Just provide Access Token (Temporary)
///
/// - `access_token`: set the access_token for this backend.
/// Please notice its expiration.
///
/// ### Or provide Client ID and Client Secret and refresh token (Long Term)
///
/// If you want to let OpenDAL to refresh the access token automatically,
/// please provide the following fields:
///
/// - `refresh_token`: set the refresh_token for dropbox api
/// - `client_id`: set the client_id for dropbox api
/// - `client_secret`: set the client_secret for dropbox api
///
/// OpenDAL is a library, it cannot do the first step of OAuth2 for you.
/// You need to get authorization code from user by calling Dropbox's authorize url
/// and exchange it for refresh token.
///
/// Please refer to [Dropbox OAuth2 Guide](https://www.dropbox.com/developers/reference/oauth-guide)
/// for more information.
///
/// You can refer to [`DropboxBuilder`]'s docs for more information.
///
/// # Example
///
/// ## Via Builder
///
/// ```rust
/// use anyhow::Result;
/// use opendal::raw::OpWrite;
/// use opendal::services::Dropbox;
/// use opendal::Operator;
///
/// #[tokio::main]
/// async fn main() -> Result<()> {
///     let mut builder = Dropbox::default();
///     builder.root("/test");
///     builder.access_token("<token>");
///
///     let op: Operator = Operator::new(builder)?.finish();
///     Ok(())
/// }
/// ```

#[derive(Default)]
pub struct DropboxBuilder {
    root: Option<String>,

    access_token: Option<String>,

    refresh_token: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,

    http_client: Option<HttpClient>,
}

impl Debug for DropboxBuilder {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Builder").finish()
    }
}

impl DropboxBuilder {
    /// Set the root directory for dropbox.
    ///
    /// Default to `/` if not set.
    pub fn root(&mut self, root: &str) -> &mut Self {
        self.root = Some(root.to_string());
        self
    }

    /// Access token is used for temporary access to the Dropbox API.
    ///
    /// You can get the access token from [Dropbox App Console](https://www.dropbox.com/developers/apps)
    ///
    /// NOTE: this token will be expired in 4 hours.
    /// If you are trying to use the Dropbox service in a long time, please set a refresh_token instead.
    pub fn access_token(&mut self, access_token: &str) -> &mut Self {
        self.access_token = Some(access_token.to_string());
        self
    }

    /// Refresh token is used for long term access to the Dropbox API.
    ///
    /// You can get the refresh token via OAuth 2.0 Flow of Dropbox.
    ///
    /// OpenDAL will use this refresh token to get a new access token when the old one is expired.
    pub fn refresh_token(&mut self, refresh_token: &str) -> &mut Self {
        self.refresh_token = Some(refresh_token.to_string());
        self
    }

    /// Set the client id for Dropbox.
    ///
    /// This is required for OAuth 2.0 Flow to refresh the access token.
    pub fn client_id(&mut self, client_id: &str) -> &mut Self {
        self.client_id = Some(client_id.to_string());
        self
    }

    /// Set the client secret for Dropbox.
    ///
    /// This is required for OAuth 2.0 Flow with refresh the access token.
    pub fn client_secret(&mut self, client_secret: &str) -> &mut Self {
        self.client_secret = Some(client_secret.to_string());
        self
    }

    /// Specify the http client that used by this service.
    ///
    /// # Notes
    ///
    /// This API is part of OpenDAL's Raw API. `HttpClient` could be changed
    /// during minor updates.
    pub fn http_client(&mut self, http_client: HttpClient) -> &mut Self {
        self.http_client = Some(http_client);
        self
    }
}

impl Builder for DropboxBuilder {
    const SCHEME: Scheme = Scheme::Dropbox;
    type Accessor = DropboxBackend;

    fn from_map(map: HashMap<String, String>) -> Self {
        let mut builder = Self::default();
        map.get("root").map(|v| builder.root(v));
        map.get("access_token").map(|v| builder.access_token(v));
        map.get("refresh_token").map(|v| builder.refresh_token(v));
        map.get("client_id").map(|v| builder.client_id(v));
        map.get("client_secret").map(|v| builder.client_secret(v));
        builder
    }

    fn build(&mut self) -> Result<Self::Accessor> {
        let root = normalize_root(&self.root.take().unwrap_or_default());
        let client = if let Some(client) = self.http_client.take() {
            client
        } else {
            HttpClient::new().map_err(|err| {
                err.with_operation("Builder::build")
                    .with_context("service", Scheme::Dropbox)
            })?
        };

        let signer = match (self.access_token.take(), self.refresh_token.take()) {
            (Some(access_token), None) => DropboxSigner {
                access_token,
                // We will never expire user specified token.
                expires_in: DateTime::<Utc>::MAX_UTC,
                ..Default::default()
            },
            (None, Some(refresh_token)) => {
                let client_id = self.client_id.take().ok_or_else(|| {
                    Error::new(
                        ErrorKind::ConfigInvalid,
                        "client_id must be set when refresh_token is set",
                    )
                    .with_context("service", Scheme::Dropbox)
                })?;
                let client_secret = self.client_secret.take().ok_or_else(|| {
                    Error::new(
                        ErrorKind::ConfigInvalid,
                        "client_secret must be set when refresh_token is set",
                    )
                    .with_context("service", Scheme::Dropbox)
                })?;

                DropboxSigner {
                    refresh_token,
                    client_id,
                    client_secret,
                    ..Default::default()
                }
            }
            (Some(_), Some(_)) => {
                return Err(Error::new(
                    ErrorKind::ConfigInvalid,
                    "access_token and refresh_token can not be set at the same time",
                )
                .with_context("service", Scheme::Dropbox))
            }
            (None, None) => {
                return Err(Error::new(
                    ErrorKind::ConfigInvalid,
                    "access_token or refresh_token must be set",
                )
                .with_context("service", Scheme::Dropbox))
            }
        };

        Ok(DropboxBackend {
            core: Arc::new(DropboxCore {
                root,
                signer: Arc::new(Mutex::new(signer)),
                client,
            }),
        })
    }
}
