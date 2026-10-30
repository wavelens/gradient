/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::{Path, PathBuf};
use std::pin::pin;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail, ensure};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::net::UnixStream;

use gradient_wire::types::GuestSize;

use crate::console::SERIAL_SOCKET;

const API_TIMEOUT: Duration = Duration::from_secs(60);

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

    pub async fn vm_add_disk(&self, path: &Path, id: &str) -> Result<PciDeviceInfo> {
        let body = disk_config(path, id);
        parse(self.put("vm.add-disk", Some(&body)).await?)
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
        tokio::time::timeout(API_TIMEOUT, self.exchange(method, endpoint, body))
            .await
            .map_err(|_| {
                anyhow!(
                    "cloud-hypervisor did not answer {endpoint} within {}s",
                    API_TIMEOUT.as_secs()
                )
            })?
    }

    async fn exchange(
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
    pub store: PathBuf,
    pub cmdline: String,
    pub cpus: u8,
    pub memory_mb: u64,
}

const STORE_DISK_ID: &str = "nix-store";

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
        let store = image.join("store.img");
        ensure!(
            store.exists(),
            "guest image {} has no store.img, build it with the current gradient.guest module",
            image.display()
        );
        Ok(GuestImage {
            kernel: image.join("kernel"),
            initrd: image.join("initrd"),
            store,
            cmdline: guest.cmdline,
            cpus: guest.cpus,
            memory_mb: guest.memory_mb,
        })
    }

    pub fn sized(&self, size: &GuestSize) -> GuestImage {
        let cpus = match size.cpus {
            0 => self.cpus,
            cpus => u8::try_from(cpus).unwrap_or(u8::MAX),
        };
        let memory_mb = match size.memory_mb {
            0 => self.memory_mb,
            memory_mb => memory_mb,
        };
        GuestImage {
            cpus,
            memory_mb,
            ..self.clone()
        }
    }

    pub fn size(&self) -> GuestSize {
        GuestSize {
            cpus: u32::from(self.cpus),
            memory_mb: self.memory_mb,
            disk_mb: 0,
        }
    }
}

fn disk_config(path: &Path, id: &str) -> serde_json::Value {
    json!({ "path": path, "id": id, "serial": id, "image_type": "Raw" })
}

fn store_disk_config(store: &Path) -> serde_json::Value {
    let mut disk = disk_config(store, STORE_DISK_ID);
    disk["readonly"] = json!(true);
    disk
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
        "disks": [store_disk_config(&image.store)],
        "vsock": { "cid": cid, "socket": dir.join("vsock.sock") },
        "serial": { "mode": "Socket", "socket": dir.join(SERIAL_SOCKET) },
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
            store: "/nix/store/x-guest/store.img".into(),
            cmdline: "console=ttyS0".into(),
            cpus: 2,
            memory_mb: 2048,
        }
    }

    #[test]
    fn the_guest_store_is_a_read_only_disk_and_no_host_share() {
        let dir = Path::new("/var/lib/gradient-worker/guests/g");
        let config = vm_config(&image(), dir, 7);

        assert_eq!(config["memory"]["shared"], true);
        assert_eq!(config["memory"]["size"], 2048_u64 * 1024 * 1024);
        assert!(config.get("fs").is_none(), "{config}");
        assert_eq!(config["disks"].as_array().map(Vec::len), Some(1));
        assert_eq!(config["disks"][0]["path"], "/nix/store/x-guest/store.img");
        assert_eq!(config["disks"][0]["readonly"], true);
        assert_eq!(config["disks"][0]["serial"], STORE_DISK_ID);
        assert_eq!(
            config["vsock"]["socket"],
            "/var/lib/gradient-worker/guests/g/vsock.sock"
        );
        assert_eq!(config["vsock"]["cid"], 7);
        assert_eq!(config["serial"]["mode"], "Socket");
        assert_eq!(
            config["serial"]["socket"],
            "/var/lib/gradient-worker/guests/g/serial.sock"
        );
    }

    #[test]
    fn a_sized_image_boots_with_the_job_size() {
        let dir = Path::new("/var/lib/gradient-worker/guests/g");
        let sized = image().sized(&GuestSize {
            cpus: 4,
            memory_mb: 768,
            ..GuestSize::default()
        });
        let config = vm_config(&sized, dir, 7);

        assert_eq!(config["cpus"]["boot_vcpus"], 4);
        assert_eq!(config["memory"]["size"], 768_u64 * 1024 * 1024);
        assert_eq!(config["payload"]["kernel"], "/nix/store/x-guest/kernel");
        assert_eq!(image().sized(&GuestSize::default()), image());
    }

    #[test]
    fn an_image_without_a_store_image_is_refused() {
        let image = tempfile::tempdir().unwrap();
        std::fs::write(
            image.path().join("guest.json"),
            r#"{"cpus":2,"memory_mb":500,"cmdline":"console=ttyS0"}"#,
        )
        .unwrap();
        let error = GuestImage::load(image.path()).unwrap_err().to_string();
        assert!(error.contains("no store.img"), "{error}");

        std::fs::write(image.path().join("store.img"), b"").unwrap();
        let loaded = GuestImage::load(image.path()).unwrap();
        assert_eq!(loaded.store, image.path().join("store.img"));
    }

    #[test]
    fn a_work_disk_is_added_as_a_raw_image() {
        let config = disk_config(Path::new("/var/lib/gradient-worker/work.img"), "work");

        assert_eq!(config["image_type"], "Raw");
        assert_eq!(config["path"], "/var/lib/gradient-worker/work.img");
        assert_eq!(config["serial"], "work");
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
