/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::{Path, PathBuf};
use std::pin::pin;

use anyhow::{Context, Result, anyhow, bail};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::net::UnixStream;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PciDeviceInfo {
    pub id: String,
    pub bdf: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct VmInfo {
    pub state: String,
}

#[derive(Clone, Debug)]
pub struct HypervisorApi {
    socket: PathBuf,
}

impl HypervisorApi {
    pub fn new(socket: PathBuf) -> Self {
        Self { socket }
    }

    pub async fn vm_create(&self, config: &serde_json::Value) -> Result<()> {
        self.put("vm.create", Some(config)).await.map(drop)
    }

    pub async fn vm_boot(&self) -> Result<()> {
        self.put("vm.boot", None).await.map(drop)
    }

    pub async fn vm_pause(&self) -> Result<()> {
        self.put("vm.pause", None).await.map(drop)
    }

    pub async fn vm_resume(&self) -> Result<()> {
        self.put("vm.resume", None).await.map(drop)
    }

    pub async fn vm_add_device(&self, path: &str, id: &str) -> Result<PciDeviceInfo> {
        let body = json!({ "path": path, "id": id });
        parse(self.put("vm.add-device", Some(&body)).await?)
    }

    pub async fn vm_add_fs(&self, tag: &str, socket: &Path, id: &str) -> Result<PciDeviceInfo> {
        let body = json!({
            "tag": tag,
            "socket": socket,
            "id": id,
            "num_queues": 1,
            "queue_size": 1024,
        });
        parse(self.put("vm.add-fs", Some(&body)).await?)
    }

    pub async fn vm_add_net(&self, tap: &str, mac: &str, id: &str) -> Result<PciDeviceInfo> {
        let body = json!({ "tap": tap, "mac": mac, "id": id });
        parse(self.put("vm.add-net", Some(&body)).await?)
    }

    pub async fn vm_remove_device(&self, id: &str) -> Result<()> {
        let body = json!({ "id": id });
        self.put("vm.remove-device", Some(&body)).await.map(drop)
    }

    pub async fn vm_shutdown(&self) -> Result<()> {
        self.put("vm.shutdown", None).await.map(drop)
    }

    pub async fn vmm_shutdown(&self) -> Result<()> {
        self.call(Method::PUT, "vmm.shutdown", None).await.map(drop)
    }

    pub async fn vm_info(&self) -> Result<VmInfo> {
        parse(self.call(Method::GET, "vm.info", None).await?)
    }

    async fn put(&self, endpoint: &str, body: Option<&serde_json::Value>) -> Result<Bytes> {
        self.call(Method::PUT, endpoint, body).await
    }

    async fn call(
        &self,
        method: Method,
        endpoint: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<Bytes> {
        let stream = UnixStream::connect(&self.socket)
            .await
            .with_context(|| format!("connect to {}", self.socket.display()))?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;

        let mut request = Request::builder()
            .method(method)
            .uri(format!("/api/v1/{endpoint}"))
            .header("Host", "localhost")
            .header("Accept", "application/json");
        let payload = match body {
            Some(body) => {
                request = request.header("Content-Type", "application/json");
                Bytes::from(serde_json::to_vec(body)?)
            }
            None => Bytes::new(),
        };
        let request = request.body(Full::new(payload))?;

        let mut exchange = pin!(async move {
            let response = sender.send_request(request).await?;
            let status = response.status();
            let body = response.into_body().collect().await?.to_bytes();
            anyhow::Ok((status, body))
        });
        let mut connection = pin!(connection);
        let (status, body) = tokio::select! {
            result = &mut exchange => result?,
            closed = &mut connection => {
                closed?;
                exchange.await?
            }
        };

        if !status.is_success() {
            bail!(
                "cloud-hypervisor {endpoint} answered {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }
        Ok(body)
    }
}

fn parse<T: DeserializeOwned>(body: Bytes) -> Result<T> {
    serde_json::from_slice(&body).map_err(|e| {
        anyhow!(
            "unexpected cloud-hypervisor answer {}: {e}",
            String::from_utf8_lossy(&body)
        )
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestImage {
    pub kernel: PathBuf,
    pub initrd: PathBuf,
    pub cmdline: String,
    pub cpus: u8,
    pub memory_mb: u64,
}

#[derive(Deserialize)]
struct GuestJson {
    cpus: u8,
    memory_mb: u64,
    cmdline: String,
}

impl GuestImage {
    pub fn load(image: &Path) -> Result<GuestImage> {
        let path = image.join("guest.json");
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let guest: GuestJson =
            serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
        Ok(GuestImage {
            kernel: image.join("kernel"),
            initrd: image.join("initrd"),
            cmdline: guest.cmdline,
            cpus: guest.cpus,
            memory_mb: guest.memory_mb,
        })
    }
}

pub fn vm_config(image: &GuestImage, dir: &Path, cid: u32) -> serde_json::Value {
    json!({
        "payload": {
            "kernel": image.kernel,
            "initramfs": image.initrd,
            "cmdline": image.cmdline,
        },
        "cpus": { "boot_vcpus": image.cpus, "max_vcpus": image.cpus },
        "memory": { "size": image.memory_mb * 1024 * 1024, "shared": true },
        "fs": [{
            "tag": "nix-store",
            "socket": dir.join("store.sock"),
            "num_queues": 1,
            "queue_size": 1024,
        }],
        "vsock": { "cid": cid, "socket": dir.join("vsock.sock") },
        "serial": { "mode": "File", "file": dir.join("console.log") },
        "console": { "mode": "Off" },
    })
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    use super::*;

    fn image() -> GuestImage {
        GuestImage {
            kernel: "/nix/store/x-guest/kernel".into(),
            initrd: "/nix/store/x-guest/initrd".into(),
            cmdline: "console=ttyS0".into(),
            cpus: 2,
            memory_mb: 2048,
        }
    }

    #[test]
    fn guest_memory_is_shared_for_virtiofs() {
        let dir = Path::new("/var/lib/gradient-worker/guests/g");
        let config = vm_config(&image(), dir, 7);

        assert_eq!(config["memory"]["shared"], true);
        assert_eq!(config["memory"]["size"], 2048_u64 * 1024 * 1024);
        assert_eq!(config["fs"][0]["tag"], "nix-store");
        assert_eq!(
            config["fs"][0]["socket"],
            "/var/lib/gradient-worker/guests/g/store.sock"
        );
        assert_eq!(
            config["vsock"]["socket"],
            "/var/lib/gradient-worker/guests/g/vsock.sock"
        );
        assert_eq!(config["vsock"]["cid"], 7);
    }

    #[tokio::test]
    async fn an_api_error_carries_the_body() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("api.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0_u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buf).await.unwrap();
                request.extend_from_slice(&buf[..n]);
            }
            stream
                .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 4\r\n\r\nboom")
                .await
                .unwrap();
        };

        let api = HypervisorApi::new(socket);
        let (result, ()) = tokio::join!(api.vm_boot(), server);
        let error = result.unwrap_err().to_string();
        assert!(error.contains("boom"), "{error}");
    }
}
