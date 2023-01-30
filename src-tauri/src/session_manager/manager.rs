use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client;
use russh::client::{Config, Handle};
use russh::kex::{CURVE25519, DH_G14_SHA1, DH_G14_SHA256, DH_G1_SHA1};
use russh_keys::key::{SignatureHash, ED25519, RSA_SHA2_256, RSA_SHA2_512, SSH_RSA};
use uuid::Uuid;

use crate::device_manager::Device;
use crate::session_manager::connection::Connection;
use crate::session_manager::handler::ClientHandler;

use crate::session_manager::{
    Error, ErrorKind, Proc, SessionManager, Shell, ShellInfo, ShellToken,
};

impl SessionManager {
    pub async fn exec(
        &self,
        device: Device,
        command: &str,
        stdin: Option<Vec<u8>>,
    ) -> Result<Vec<u8>, Error> {
        loop {
            let conn = self.conn_obtain(device.clone()).await?;
            match conn.exec(command, &stdin).await {
                Ok(data) => return Ok(data),
                Err(e) => match e.kind {
                    ErrorKind::NeedsReconnect => {
                        log::info!("retry connection");
                        continue;
                    }
                    _ => return Err(e),
                },
            };
        }
    }

    pub async fn spawn(&self, device: Device, command: &str) -> Result<Proc, Error> {
        loop {
            let conn = self.conn_obtain(device.clone()).await?;
            match conn.spawn(command).await {
                Ok(data) => return Ok(data),
                Err(e) => match e.kind {
                    ErrorKind::NeedsReconnect => {
                        log::info!("retry connection");
                        continue;
                    }
                    _ => return Err(e),
                },
            };
        }
    }

    pub async fn shell_open(
        &self,
        device: Device,
        cols: u16,
        rows: u16,
    ) -> Result<Arc<Shell>, Error> {
        loop {
            let conn = self.conn_obtain(device.clone()).await?;
            match conn.shell(cols, rows).await {
                Ok(data) => {
                    let shell = Arc::new(data);
                    self.shells
                        .lock()
                        .unwrap()
                        .insert(shell.token.clone(), shell.clone());
                    return Ok(shell);
                }
                Err(e) => match e.kind {
                    ErrorKind::NeedsReconnect => {
                        log::info!("retry connection");
                        continue;
                    }
                    _ => return Err(e),
                },
            }
        }
    }

    pub async fn shell_close(&self, token: &ShellToken) -> Result<(), Error> {
        let shell = self.shells.lock().unwrap().remove(&token).clone();
        if let Some(shell) = shell {
            let shell = shell.clone();
            tokio::spawn(async move {
                shell.close().await.unwrap_or(());
            });
        }
        return Ok(());
    }

    pub fn shell_find(&self, token: &ShellToken) -> Result<Arc<Shell>, Error> {
        return self
            .shells
            .lock()
            .unwrap()
            .get(token)
            .map(|a| a.clone())
            .ok_or_else(|| Error {
                message: String::from("No shell"),
                kind: ErrorKind::NotFound,
            });
    }

    pub fn shell_list(&self) -> Vec<ShellInfo> {
        let mut list: Vec<ShellInfo> = self
            .shells
            .lock()
            .unwrap()
            .iter()
            .map(|(_, shell)| shell.info())
            .collect();
        list.sort_by_key(|v| v.created_at);
        return list;
    }

    async fn conn_obtain(&self, device: Device) -> Result<Arc<Connection>, Error> {
        if device.new {
            return Ok(Arc::new(self.conn_new(device.clone()).await?));
        }
        let guard = self.lock.lock().await;
        if let Some(a) = self.connections.lock().unwrap().get(&device.name) {
            return Ok(a.clone());
        }
        let connection = Arc::new(self.conn_new(device.clone()).await?);
        log::info!("Connection to {} has been created", device.name);
        self.connections
            .lock()
            .unwrap()
            .insert(device.name, connection.clone());
        drop(guard);
        return Ok(connection);
    }

    async fn conn_new(&self, device: Device) -> Result<Connection, Error> {
        let id = Uuid::new_v4();
        let (mut handle, sig_alg) = match self.try_conn(&id, &device, false).await {
            Ok(v) => v,
            Err(_e @ russh::Error::KexInit)
            | Err(_e @ russh::Error::NoCommonKexAlgo)
            | Err(_e @ russh::Error::NoCommonKeyAlgo)
            | Err(_e @ russh::Error::NoCommonCipher) => self.try_conn(&id, &device, true).await?,
            e => e?,
        };
        log::debug!("Connected to {}, sig_alg: {:?}", device.name, sig_alg);
        if let Some(key) = &device.private_key {
            let key = Arc::new(key.priv_key(device.passphrase.as_deref(), sig_alg)?);
            log::debug!("Key algorithm: {:?}", key.name());
            if !handle.authenticate_publickey(&device.username, key).await? {
                return Err(Error {
                    message: format!("Device refused pubkey authorization"),
                    kind: ErrorKind::Authorization,
                });
            }
        } else if let Some(password) = &device.password {
            if !handle
                .authenticate_password(&device.username, password)
                .await?
            {
                return Err(Error {
                    message: format!("Device refused password authorization"),
                    kind: ErrorKind::Authorization,
                });
            }
        } else if !handle.authenticate_none(&device.username).await? {
            return Err(Error {
                message: format!("Device refused authorization"),
                kind: ErrorKind::Authorization,
            });
        }
        log::debug!("Authenticated to {}", device.name);
        return Ok(Connection::new(
            id,
            device,
            handle,
            Arc::downgrade(&self.connections),
        ));
    }

    async fn try_conn(
        &self,
        id: &Uuid,
        device: &Device,
        legacy_algo: bool,
    ) -> Result<(Handle<ClientHandler>, Option<SignatureHash>), russh::Error> {
        let mut config = Config::default();
        if legacy_algo {
            config.preferred.key = &[SSH_RSA, RSA_SHA2_512, RSA_SHA2_256, ED25519];
            config.preferred.kex = &[DH_G14_SHA1, DH_G1_SHA1, DH_G14_SHA256, CURVE25519];
        }
        config.connection_timeout = Some(Duration::from_secs(3));
        let server_sig_alg: Arc<Mutex<Option<SignatureHash>>> = Arc::new(Mutex::default());
        let handler = ClientHandler {
            id: id.clone(),
            key: device.name.clone(),
            connections: Arc::downgrade(&self.connections),
            shells: Arc::downgrade(&self.shells),
            sig_alg: server_sig_alg.clone(),
        };
        let addr = SocketAddr::from_str(&format!("{}:{}", &device.host, &device.port)).unwrap();
        log::debug!("Connecting to {}", addr);
        let handle = client::connect(Arc::new(config), addr, handler).await?;
        return Ok((handle, server_sig_alg.lock().unwrap().clone()));
    }
}