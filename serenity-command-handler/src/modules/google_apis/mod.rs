use std::{collections::HashMap, path::Path, sync::Arc};

use anyhow::Context;
use base64::{Engine, prelude::BASE64_URL_SAFE};
use chrono::{DateTime, Duration, Utc};
use rsa::{Pkcs1v15Sign, RsaPrivateKey, pkcs8::DecodePrivateKey};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use tokio::sync::RwLock;

pub mod sheets;

#[derive(Deserialize)]
#[allow(unused)]
pub struct Credentials {
    #[serde(rename = "type")]
    typ: String,
    project_id: String,
    private_key_id: String,
    private_key: String,
    client_email: String,
    client_id: String,
    auth_uri: String,
    token_uri: String,
    auth_provider_x509_cert_url: String,
    client_x509_cert_url: String,
}

#[derive(Serialize)]
enum JWTAlg {
    RS256,
}

#[derive(Serialize)]
enum JWTType {
    JWT,
}

#[derive(Serialize)]
struct JWTHeader<'a> {
    alg: JWTAlg,
    typ: JWTType,
    kid: &'a str,
}

#[derive(Serialize)]

struct JWTClaims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    exp: i64,
    iat: i64,
}

#[derive(Deserialize)]
struct AuthTokenResponse {
    access_token: String,
    token_type: String,
    expires_in: i64,
}

#[derive(Debug)]
#[allow(unused)]
pub struct AccessToken {
    access_token: String,
    token_type: String,
    exp: DateTime<Utc>,
}

impl Credentials {
    pub fn from_str(serialized: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(serialized)
    }

    pub fn from_file<P: AsRef<Path>>(filename: P) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(filename.as_ref())?;
        Self::from_str(&contents).map_err(Into::into)
    }

    fn build_jwt(&self, scopes: &[&str]) -> anyhow::Result<String> {
        let header = JWTHeader {
            alg: JWTAlg::RS256,
            typ: JWTType::JWT,
            kid: &self.private_key_id,
        };
        let now = Utc::now();
        let exp = now + Duration::hours(1);
        let claims = JWTClaims {
            iss: &self.client_email,
            scope: &scopes.join(" "),
            aud: &self.token_uri,
            exp: exp.timestamp(),
            iat: now.timestamp(),
        };
        let header_encoded = BASE64_URL_SAFE.encode(serde_json::to_string(&header)?);
        let claims_encoded = BASE64_URL_SAFE.encode(serde_json::to_string(&claims)?);
        let base = format!("{header_encoded}.{claims_encoded}");
        let hashed = sha2::Sha256::digest(&base);

        let key = RsaPrivateKey::from_pkcs8_pem(&self.private_key)
            .context("failed to read private key")?;
        let pkcs1_15 = Pkcs1v15Sign::new::<sha2::Sha256>();
        let signature = key.sign(pkcs1_15, &hashed).context("failed to sign JWT")?;

        let jwt = format!("{base}.{}", BASE64_URL_SAFE.encode(&signature));
        Ok(jwt)
    }

    pub async fn request_token(&self, scopes: &[&str]) -> anyhow::Result<AccessToken> {
        let token = self.build_jwt(scopes)?;
        let client = reqwest::Client::new();
        let params = HashMap::from([
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", &token),
        ]);
        let req = client.post(&self.token_uri).form(&params);
        let AuthTokenResponse {
            access_token,
            token_type,
            expires_in,
        } = req.send().await?.json().await?;

        Ok(AccessToken {
            access_token,
            token_type,
            exp: Utc::now() + Duration::seconds(expires_in),
        })
    }

    pub fn authenticator(self: &Arc<Self>, scopes: &'static [&'static str]) -> Authenticator {
        Authenticator {
            credentials: Arc::clone(self),
            scopes,
            token: RwLock::default(),
        }
    }
}

impl AccessToken {
    fn is_valid(&self) -> bool {
        self.exp - Utc::now() > Duration::minutes(1)
    }
}

pub struct Authenticator {
    credentials: Arc<Credentials>,
    scopes: &'static [&'static str],
    token: RwLock<Option<AccessToken>>,
}

impl Authenticator {
    pub async fn get_token(&self) -> anyhow::Result<String> {
        if let Some(token) = self.token.read().await.as_ref()
            && token.is_valid()
        {
            return Ok(token.access_token.clone());
        }
        let mut write_lock = self.token.write().await;
        // check if token has already been refreshed while waiting for the lock
        if let Some(token) = write_lock.as_ref()
            && token.is_valid()
        {
            return Ok(token.access_token.clone());
        }

        let new_token = self.credentials.request_token(&self.scopes).await?;
        let res = new_token.access_token.clone();
        *write_lock = Some(new_token);
        Ok(res)
    }
}
