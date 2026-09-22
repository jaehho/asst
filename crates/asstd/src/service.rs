//! `dev.jaeho.Asst1` on the session bus.

use std::sync::Arc;

use asst_core::api::{AddSpec, Change, ListChange, ListSpec, SettingsChange};
use asst_core::store::Query;
use serde::Serialize;
use serde::de::DeserializeOwned;
use zbus::object_server::SignalEmitter;
use zbus::{fdo, interface};

use crate::daemon::{Daemon, Event};

pub struct Service {
    pub daemon: Arc<Daemon>,
}

fn failed(e: impl std::fmt::Display) -> fdo::Error {
    fdo::Error::Failed(e.to_string())
}

fn json<T: Serialize>(v: &T) -> fdo::Result<String> {
    serde_json::to_string(v).map_err(failed)
}

fn arg<T: DeserializeOwned>(s: &str) -> fdo::Result<T> {
    serde_json::from_str(s).map_err(|e| fdo::Error::InvalidArgs(e.to_string()))
}

#[interface(name = "dev.jaeho.Asst1")]
impl Service {
    async fn status(&self) -> fdo::Result<String> {
        json(&self.daemon.status())
    }

    async fn lists(&self) -> fdo::Result<String> {
        json(&self.daemon.lists().map_err(failed)?)
    }

    async fn tasks(&self, query: &str) -> fdo::Result<String> {
        let q: Query = arg(query)?;
        json(&self.daemon.tasks(&q).map_err(failed)?)
    }

    async fn get(&self, id: &str) -> fdo::Result<String> {
        json(&self.daemon.get(id).map_err(failed)?)
    }

    async fn add(&self, spec: &str) -> fdo::Result<String> {
        let spec: AddSpec = arg(spec)?;
        json(&self.daemon.add(&spec).map_err(failed)?)
    }

    async fn parse(&self, text: &str) -> fdo::Result<String> {
        json(&self.daemon.parse(text).map_err(failed)?)
    }

    async fn edit(&self, id: &str, change: &str) -> fdo::Result<String> {
        let change: Change = arg(change)?;
        json(&self.daemon.edit(id, &change).map_err(failed)?)
    }

    async fn complete(&self, id: &str) -> fdo::Result<String> {
        json(&self.daemon.complete(id).map_err(failed)?)
    }

    async fn reopen(&self, id: &str) -> fdo::Result<String> {
        json(&self.daemon.reopen(id).map_err(failed)?)
    }

    async fn delete(&self, id: &str) -> fdo::Result<()> {
        self.daemon.delete(id).map_err(failed)
    }

    async fn ics(&self, id: &str) -> fdo::Result<String> {
        self.daemon.ics(id).map_err(failed)
    }

    async fn sync(&self) -> fdo::Result<String> {
        match self.daemon.sync_and_wait().await {
            Ok(report) => json(&report),
            Err(e) => Err(failed(e)),
        }
    }

    async fn login(&self, server: &str) -> fdo::Result<String> {
        self.daemon.start_login(server).await.map_err(failed)
    }

    async fn login_password(
        &self,
        server: &str,
        username: &str,
        password: &str,
    ) -> fdo::Result<String> {
        self.daemon
            .login_with_password(server, username, password)
            .await
            .map_err(failed)?;
        json(&self.daemon.status())
    }

    async fn logout(&self) -> fdo::Result<()> {
        self.daemon.logout().await.map_err(failed)
    }

    async fn duplicate(&self, id: &str) -> fdo::Result<String> {
        json(&self.daemon.duplicate(id).map_err(failed)?)
    }

    async fn create_list(&self, spec: &str) -> fdo::Result<String> {
        let spec: ListSpec = arg(spec)?;
        json(&self.daemon.create_list(&spec).await.map_err(failed)?)
    }

    async fn update_list(&self, href: &str, change: &str) -> fdo::Result<String> {
        let change: ListChange = arg(change)?;
        json(
            &self
                .daemon
                .update_list(href, &change)
                .await
                .map_err(failed)?,
        )
    }

    async fn delete_list(&self, href: &str) -> fdo::Result<()> {
        self.daemon.delete_list(href).await.map_err(failed)
    }

    async fn delete_completed(&self, list: &str) -> fdo::Result<u32> {
        let list = Some(list).filter(|l| !l.is_empty());
        let n = self.daemon.delete_completed(list).map_err(failed)?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    }

    async fn link(&self, list: &str, repo: &str) -> fdo::Result<String> {
        json(&self.daemon.link(list, repo).map_err(failed)?)
    }

    async fn unlink(&self, repo: &str) -> fdo::Result<bool> {
        self.daemon.unlink(repo).map_err(failed)
    }

    async fn links(&self) -> fdo::Result<String> {
        json(&self.daemon.links().map_err(failed)?)
    }

    async fn settings(&self) -> fdo::Result<String> {
        json(&self.daemon.settings())
    }

    async fn set_settings(&self, change: &str) -> fdo::Result<String> {
        let change: SettingsChange = arg(change)?;
        json(&self.daemon.set_settings(&change).map_err(failed)?)
    }

    #[zbus(signal)]
    async fn changed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn status_changed(emitter: &SignalEmitter<'_>, status: &str) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn login_done(emitter: &SignalEmitter<'_>, ok: bool, message: &str) -> zbus::Result<()>;
}

/// Turn daemon events into signals.
pub async fn pump(conn: zbus::Connection, mut events: tokio::sync::mpsc::UnboundedReceiver<Event>) {
    let emitter = match SignalEmitter::new(&conn, asst_core::api::OBJECT_PATH) {
        Ok(e) => e,
        Err(e) => {
            log::error!("signal emitter: {e}");
            return;
        }
    };
    while let Some(first) = events.recv().await {
        let mut batch = vec![first];
        while let Ok(more) = events.try_recv() {
            batch.push(more);
        }
        // A burst of edits is one Changed, sent last.
        let mut changed = false;
        for event in batch {
            let result = match event {
                Event::Changed => {
                    changed = true;
                    Ok(())
                }
                Event::Status(s) => {
                    Service::status_changed(
                        &emitter,
                        &serde_json::to_string(&s).unwrap_or_default(),
                    )
                    .await
                }
                Event::LoginDone(ok, message) => Service::login_done(&emitter, ok, &message).await,
            };
            if let Err(e) = result {
                log::warn!("emitting a signal: {e}");
            }
        }
        if changed && let Err(e) = Service::changed(&emitter).await {
            log::warn!("emitting a signal: {e}");
        }
    }
}
