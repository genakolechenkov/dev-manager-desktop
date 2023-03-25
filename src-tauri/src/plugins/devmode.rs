use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use tauri::plugin::{Builder, TauriPlugin};
use tauri::regex::Regex;
use tauri::{AppHandle, Manager, Runtime, State};

use crate::device_manager::{Device, DeviceManager};
use crate::error::Error;
use crate::session_manager::SessionManager;

#[derive(Serialize)]
pub struct DevModeStatus {
    token: Option<String>,
    remaining: Option<String>,
}

#[derive(Deserialize)]
struct DevModeSession {
    result: String,
    #[serde(rename = "errorCode")]
    error_code: Option<String>,
    #[serde(rename = "errorMsg")]
    error_msg: Option<String>,
}

#[tauri::command]
async fn token<R: Runtime>(app: AppHandle<R>, device: Device) -> Result<String, Error> {
    if device.username != "prisoner" {
        return Err(Error::Unsupported);
    }
    if let Some(token) = valid_token(app, device).await? {
        return Ok(token);
    }
    return Err(Error::Unsupported);
}

#[tauri::command]
async fn status<R: Runtime>(app: AppHandle<R>, device: Device) -> Result<DevModeStatus, Error> {
    if let Some(token) = valid_token(app, device).await? {
        let url = Url::parse_with_params(
            "https://developer.lge.com/secure/CheckDevModeSession.dev",
            &[("sessionToken", token.clone())],
        )
        .expect("should be valid url");
        let session: DevModeSession = reqwest::get(url).await?.json().await?;
        if session.result == "success" {
            return Ok(DevModeStatus {
                token: Some(token),
                remaining: Some(session.error_msg.unwrap_or(String::from(""))),
            });
        }
        return Ok(DevModeStatus {
            token: Some(token),
            remaining: None,
        });
    }
    return Ok(DevModeStatus {
        token: None,
        remaining: None,
    });
}

async fn valid_token<R: Runtime>(
    app: AppHandle<R>,
    device: Device,
) -> Result<Option<String>, Error> {
    let data = tokio::task::spawn_blocking(move || {
        let sessions = app.state::<SessionManager>();
        let pool = sessions.pool(device);
        let session = pool.get()?;
        let sftp = session.sftp()?;
        let mut ch = sftp.open("/var/luna/preferences/devmode_enabled", 0, 0)?;
        let mut data = Vec::<u8>::new();
        ch.read_to_end(&mut data)?;
        return Ok::<Vec<u8>, Error>(data);
    })
    .await
    .unwrap()?;
    let token = String::from_utf8(data).map_err(|_| Error::IO {
        code: format!("Other"),
        message: format!("Can\'t read dev mode token"),
    })?;
    let regex = Regex::new("^[0-9a-zA-Z]+$").unwrap();
    if !regex.is_match(&token) {
        log::debug!("Token {} doesn't look like a valid devmode token", token);
        return Ok(None);
    }
    return Ok(Some(token));
}

/// Initializes the plugin.
pub fn plugin<R: Runtime>(name: &'static str) -> TauriPlugin<R> {
    Builder::new(name)
        .invoke_handler(tauri::generate_handler![status, token])
        .build()
}
