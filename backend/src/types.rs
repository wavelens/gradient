use uuid::Uuid;
use sea_orm::DatabaseConnection;
use nix_daemon::nix::DaemonStore;
use async_ssh2_lite::{ AsyncChannel, TokioTcpStream };

pub type ListResponse = Vec<(Uuid, String)>;
pub type NixStore = DaemonStore<AsyncChannel<TokioTcpStream>>;

#[derive(Clone)]
pub struct AppState {
    pub conn: DatabaseConnection,
}

