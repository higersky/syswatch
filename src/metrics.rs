use crate::nvml_metrics::{NvmlDevice, NvmlMetricsCollector, NvmlUserUtilization};
use anyhow::Context;
use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::family::Family;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::atomic::AtomicU64;

use anyhow::Result;

use prometheus_client::metrics::gauge::Gauge;


#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct DeviceLabel {
    pub index: u32,
    pub minor_number: u32,
    pub name: String,
    pub uuid: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct UserLabel {
    pub index: u32,
    pub user_name: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct VersionLabel {
    pub version: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct DeviceMinorLabel {
    pub minor_number: u32,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct UserNameLabel {
    pub user_name: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct WatchdogLabel {
    pub hostname: String,
    pub url: String,
}

#[derive(Deserialize, Debug)]
pub struct KeepAliveConfig {
    pub interval: u64,
    pub item: Vec<KeepAliveItem>,
}

#[derive(Deserialize, Debug)]
pub struct KeepAliveItem {
    pub hostname: String,
    pub url: String,
}

#[derive(Default)]
pub struct Metrics {
    pub nvml_status: Gauge,
    pub version: Family<VersionLabel, Gauge>,
    pub device_info: Family<DeviceLabel, Gauge>,
    pub fan_speed: Family<DeviceMinorLabel, Gauge>,
    pub memory_total: Family<DeviceMinorLabel, Gauge>,
    pub memory_used: Family<DeviceMinorLabel, Gauge>,
    pub power_usage: Family<DeviceMinorLabel, Gauge>,
    pub temperature: Family<DeviceMinorLabel, Gauge>,
    pub utilization_gpu: Family<DeviceMinorLabel, Gauge<f64, AtomicU64>>,
    pub utilization_memory: Family<DeviceMinorLabel, Gauge<f64, AtomicU64>>,
    pub users_used_memory: Family<UserLabel, Gauge>,
    pub users_used_cards: Family<UserNameLabel, Gauge>,
}

#[derive(Default)]
pub struct AliveStatus {
    pub alive_status: Family<WatchdogLabel, Gauge>,
}

impl Metrics {
    pub fn new() -> Metrics {
        Default::default()
    }

    pub fn clear(&self) {
        self.version.clear();
        self.device_info.clear();
        self.fan_speed.clear();
        self.memory_total.clear();
        self.memory_used.clear();
        self.power_usage.clear();
        self.temperature.clear();
        self.utilization_gpu.clear();
        self.utilization_memory.clear();
        self.users_used_memory.clear();
        self.users_used_cards.clear();
    }

    pub fn update(&self, collector: &mut NvmlMetricsCollector) -> Result<()> {
        let state = collector
            .now()
            .with_context(|| {
                self.nvml_status.set(0);
                "Failed to update metrics"
            })?;

        self.update_nvml_version(state.version);

        for device in state.devices {
            self.update_nvml_device(device);
        }

        self.users_used_memory.clear();
        let mut count = HashMap::new();
        for user in state.users_utilization.iter() {
            if user.used_gpu_memory != 0 {
                count
                    .entry(user.user_name.clone())
                    .and_modify(|x: &mut i64| *x += 1)
                    .or_insert(1);
                self.update_nvml_user_utilization(user);
            }
        }

        self.users_used_cards.clear();
        for (user_name, cnt) in count {
            self.users_used_cards
                .get_or_create(&UserNameLabel {
                    user_name: user_name.clone(),
                })
                .set(cnt);
        }
        
        self.nvml_status.set(1);
        Ok(())
    }

    fn update_nvml_user_utilization(&self, user: &NvmlUserUtilization) {
        let ulabel = UserLabel {
            user_name: user.user_name.clone(),
            index: user.index,
        };
        self.users_used_memory
            .get_or_create(&ulabel)
            .set(user.used_gpu_memory as i64);
    }

    fn update_nvml_version(&self, version: String) {
        self.version.get_or_create(&VersionLabel { version }).set(1);
    }

    fn update_nvml_device(&self, device: NvmlDevice) {
        self.device_info
            .get_or_create(&DeviceLabel {
                index: device.index,
                minor_number: device.minor_number,
                name: device.name,
                uuid: device.uuid,
            })
            .set(1);
        let mlabel = DeviceMinorLabel {
            minor_number: device.minor_number,
        };
        self.fan_speed
            .get_or_create(&mlabel)
            .set(device.fan_speed.into());
        self.memory_total
            .get_or_create(&mlabel)
            .set(device.memory_total as i64);
        self.memory_used
            .get_or_create(&mlabel)
            .set(device.memory_used as i64);
        self.power_usage
            .get_or_create(&mlabel)
            .set(device.power_usage.into());
        self.temperature
            .get_or_create(&mlabel)
            .set(device.temperature.into());
        self.utilization_gpu
            .get_or_create(&mlabel)
            .set((device.utilization_gpu as f64) / 100.);
        self.utilization_memory
            .get_or_create(&mlabel)
            .set((device.utilization_memory as f64) / 100.);
    }
}

impl AliveStatus {
    pub fn update(&self, item: &KeepAliveItem, status: bool) {
        self.alive_status
            .get_or_create(&WatchdogLabel {
                hostname: item.hostname.clone(),
                url: item.url.clone(),
            })
            .set(status as i64);
    }
}
