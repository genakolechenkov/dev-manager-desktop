use async_trait::async_trait;
use russh::client::Msg;
use russh::{Channel, ChannelMsg, CryptoVec, Sig};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use tokio::sync::MutexGuard;

use crate::error::Error;
use crate::session_manager::connection::Connection;
use crate::session_manager::spawned::Spawned;
use crate::session_manager::Proc;

impl Proc {
    pub async fn start(&self) -> Result<(), Error> {
        if let Some(ch) = self.ch.lock().await.as_mut() {
            ch.exec(true, self.command.as_bytes()).await?;
            if !Connection::wait_reply(ch).await? {
                return Err(Error::NegativeReply);
            }
        }
        return Ok(());
    }

    pub fn signal(&self, signal: Sig) -> Result<(), Error> {
        return if let Some(sender) = self.sender.lock().unwrap().as_mut() {
            sender
                .send(ChannelMsg::Signal { signal })
                .map_err(|_| Error::Disconnected)?;
            sender
                .send(ChannelMsg::Eof)
                .map_err(|_| Error::Disconnected)?;
            return Ok(());
        } else {
            log::info!("Failed to send signal{:?}: disconnected", signal);
            Err(Error::Disconnected)
        };
    }

    pub fn data(&self, data: &[u8]) -> Result<(), Error> {
        return if let Some(sender) = self.sender.lock().unwrap().as_mut() {
            return sender
                .send(ChannelMsg::Data {
                    data: CryptoVec::from_slice(data),
                })
                .map_err(|_| Error::Disconnected);
        } else {
            log::info!("Failed to send data {:?}: disconnected", data);
            Err(Error::Disconnected)
        };
    }
}

#[async_trait]
impl Spawned for Proc {
    async fn lock_channel(&self) -> MutexGuard<'_, Option<Channel<Msg>>> {
        return self.ch.lock().await;
    }

    fn tx_ready(&self, sender: UnboundedSender<ChannelMsg>) {
        *self.sender.lock().unwrap() = Some(sender);
    }

    fn on_rx(&self, data: CryptoVec, ext: u32) {
        if let Some(cb) = self.callback.lock().unwrap().as_deref() {
            cb.rx(ext, data.as_ref());
        }
    }

    async fn send_msg(&self, ch: &mut Channel<Msg>, msg: ChannelMsg) -> Result<(), Error> {
        return match msg {
            ChannelMsg::Signal { signal } => Ok(ch.signal(signal).await?),
            ChannelMsg::Eof => Ok(ch.eof().await?),
            _ => unimplemented!(),
        };
    }
}
