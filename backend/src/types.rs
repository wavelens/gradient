use uuid::Uuid;
use sea_orm::DatabaseConnection;
use nix_daemon::nix::DaemonStore;
use async_ssh2_lite::{ AsyncChannel, TokioTcpStream };
use std::sync::Arc;

pub type ListResponse = Vec<(Uuid, String)>;
pub type NixStore = DaemonStore<AsyncChannel<TokioTcpStream>>;
pub type DBConn = Arc<DatabaseConnection>;

#[derive(Clone)]
pub struct AppState {
    pub conn: Arc<DatabaseConnection>,
}

