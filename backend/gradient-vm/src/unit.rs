/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

use anyhow::{Context, Result, bail};
use zbus::zvariant::{OwnedObjectPath, Value};

use crate::state::GuestId;

const UNIT_PREFIX: &str = "gradient-guest-";
const UNIT_SUFFIX: &str = ".service";
const SLICE: &str = "gradient-guests.slice";
const STOP_WAIT: Duration = Duration::from_secs(60);

type UnitStatus = (
    String,
    String,
    String,
    String,
    String,
    String,
    OwnedObjectPath,
    u32,
    String,
    OwnedObjectPath,
);

#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait SystemdManager {
    fn start_transient_unit(
        &self,
        name: &str,
        mode: &str,
        properties: &[(&str, Value<'_>)],
        aux: &[(&str, &[(&str, Value<'_>)])],
    ) -> zbus::Result<OwnedObjectPath>;

    fn stop_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;

    fn list_units_by_patterns(
        &self,
        states: &[&str],
        patterns: &[&str],
    ) -> zbus::Result<Vec<UnitStatus>>;
}

pub fn unit_name(id: &GuestId) -> String {
    format!("{UNIT_PREFIX}{id}{UNIT_SUFFIX}")
}

pub fn guest_id_of(unit: &str) -> Option<GuestId> {
    unit.strip_prefix(UNIT_PREFIX)?
        .strip_suffix(UNIT_SUFFIX)?
        .parse()
        .ok()
}

#[derive(Clone)]
pub struct GuestUnits {
    manager: SystemdManagerProxy<'static>,
}

impl GuestUnits {
    pub async fn connect() -> Result<Self> {
        let bus = zbus::Connection::system()
            .await
            .context("connect to the system bus")?;
        let manager = SystemdManagerProxy::new(&bus).await?;
        Ok(Self { manager })
    }

    pub async fn start(&self, id: &GuestId, argv: Vec<String>) -> Result<()> {
        let Some(program) = argv.first().cloned() else {
            bail!("guest unit without a command");
        };
        let name = unit_name(id);
        let exec_start = vec![(program, argv, false)];
        let mut properties = vec![
            ("Description", Value::from(format!("Gradient guest {id}"))),
            ("ExecStart", Value::from(exec_start)),
            ("UMask", Value::from(0o007_u32)),
            ("LimitMEMLOCK", Value::from(u64::MAX)),
            ("KillMode", Value::from("control-group")),
            ("CollectMode", Value::from("inactive-or-failed")),
            ("Slice", Value::from(SLICE)),
        ];
        #[expect(
            clippy::disallowed_methods,
            reason = "the guest unit finds ip, usbip and virtiofsd through the worker's PATH"
        )]
        if let Ok(path) = std::env::var("PATH") {
            properties.push(("Environment", Value::from(vec![format!("PATH={path}")])));
        }
        self.manager
            .start_transient_unit(&name, "fail", &properties, &[])
            .await
            .with_context(|| format!("start {name}"))?;
        Ok(())
    }

    pub async fn stop(&self, id: &GuestId) -> Result<()> {
        let name = unit_name(id);
        match self.manager.stop_unit(&name, "replace").await {
            Ok(_) => {}
            Err(zbus::Error::MethodError(error, _, _))
                if error.as_str() == "org.freedesktop.systemd1.NoSuchUnit" =>
            {
                return Ok(());
            }
            Err(e) => return Err(e).with_context(|| format!("stop {name}")),
        }
        self.wait_inactive(&name).await
    }

    pub async fn list(&self) -> Result<Vec<(GuestId, bool)>> {
        let pattern = format!("{UNIT_PREFIX}*{UNIT_SUFFIX}");
        let units = self
            .manager
            .list_units_by_patterns(&[], &[&pattern])
            .await
            .context("list guest units")?;
        Ok(units
            .into_iter()
            .filter_map(|unit| Some((guest_id_of(&unit.0)?, is_active(&unit.3))))
            .collect())
    }

    async fn wait_inactive(&self, name: &str) -> Result<()> {
        let deadline = tokio::time::Instant::now() + STOP_WAIT;
        loop {
            let units = self.manager.list_units_by_patterns(&[], &[name]).await?;
            if !units.iter().any(|unit| is_active(&unit.3)) {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                bail!("{name} still active after {}s", STOP_WAIT.as_secs());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
}

fn is_active(active_state: &str) -> bool {
    matches!(
        active_state,
        "active" | "activating" | "reloading" | "deactivating"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_unit_name_maps_back_to_its_guest() {
        let id = GuestId::new();
        assert_eq!(guest_id_of(&unit_name(&id)), Some(id));
    }

    #[test]
    fn foreign_units_are_not_guests() {
        let id = GuestId::new();
        for unit in [
            "gradient-guest-.service".to_owned(),
            "gradient-guest-xyz.service".to_owned(),
            "gradient-worker.service".to_owned(),
            format!("gradient-guest-{id}.socket"),
        ] {
            assert_eq!(guest_id_of(&unit), None, "{unit}");
        }
    }
}
