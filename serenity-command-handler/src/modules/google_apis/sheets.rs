use std::sync::Arc;

use anyhow::{Context, bail};
use reqwest::Url;
use serde::{Deserialize, Serialize};

use super::{Authenticator, Credentials};

pub const SCOPE_SPREADSHEETS: &str = "https://www.googleapis.com/auth/spreadsheets";

const BASE: &str = "https://sheets.googleapis.com";
const PATH_BASE: &str = "/v4/spreadsheets";

pub struct Sheets {
    authenticator: Authenticator,
    client: reqwest::Client,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ValueRange {
    #[serde(rename = "majorDimension")]
    pub major_dimension: Option<String>,
    pub range: Option<String>,
    pub values: Option<Vec<Vec<serde_json::Value>>>,
}

impl Sheets {
    pub fn new(credentials: &Arc<Credentials>) -> Self {
        let authenticator = credentials.authenticator(&[SCOPE_SPREADSHEETS]);
        Sheets {
            authenticator,
            client: reqwest::Client::new(),
        }
    }

    pub async fn get_range(&self, sheet_id: &str, range: &str) -> anyhow::Result<ValueRange> {
        let token = self.authenticator.get_token().await?;
        let mut url = Url::parse(BASE)?;
        url.set_path(&format!("{PATH_BASE}/{sheet_id}/values/{range}"));
        let resp = self
            .client
            .get(url)
            .header("Authorization", &format!("Bearer {token}"))
            .send()
            .await?;
        if !resp.status().is_success() {
            let body = resp.text().await?;
            bail!(body);
        }

        resp.json().await.context("failed to parse response body")
    }

    async fn do_update_range(
        &self,
        sheet_id: &str,
        range: &str,
        values: ValueRange,
        append: bool,
    ) -> anyhow::Result<()> {
        let token = self.authenticator.get_token().await?;

        let suff_append = if append { ":append" } else { "" };
        let mut url = Url::parse(BASE)?;
        url.set_path(&format!(
            "{PATH_BASE}/{sheet_id}/values/{range}{suff_append}"
        ));
        let body = serde_json::to_string(&values)?;
        let resp = self
            .client
            .post(url)
            .header("Authorization", &format!("Bearer {token}"))
            .query(&[("valueInputOption", "USER_ENTERED")])
            .body(body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let body = resp.text().await?;
            bail!(body);
        }

        resp.json().await.context("failed to parse response body")
    }

    pub async fn update_range(
        &self,
        sheet_id: &str,
        range: &str,
        values: ValueRange,
    ) -> anyhow::Result<()> {
        self.do_update_range(sheet_id, range, values, false).await
    }

    pub async fn append_range(
        &self,
        sheet_id: &str,
        range: &str,
        values: ValueRange,
    ) -> anyhow::Result<()> {
        self.do_update_range(sheet_id, range, values, true).await
    }
}
