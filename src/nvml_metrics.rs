use crate::utils;
use anyhow::Context;
use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use nvml_wrapper::enums::device::UsedGpuMemory;
use nvml_wrapper::Nvml;
use std::collections::HashMap;
use users::{uid_t, User};

#[derive(Debug)]
pub struct NvmlMetrics {
    pub version: String,
    pub devices: Vec<NvmlDevice>,
    pub users_utilization: Vec<NvmlUserUtilization>,
}

#[derive(Debug)]
pub struct NvmlDevice {
    pub index: u32,
    pub minor_number: u32,
    pub name: String,
    pub uuid: String,
    pub temperature: u32,
    pub power_usage: u32,
    pub fan_speed: u32,
    pub memory_total: u64,
    pub memory_used: u64,
    pub utilization_memory: u32,
    pub utilization_gpu: u32,
}

#[derive(Debug)]
pub struct NvmlUserUtilization {
    pub index: u32,
    pub user_name: String,
    pub used_gpu_memory: u64,
}

pub struct NvmlMetricsCollector {
    nvml: Nvml,
    show_all_users: bool,
    known_user_map: HashMap<uid_t, User>,
    blocked_user_map: HashMap<uid_t, User>,
}

impl NvmlMetricsCollector {
    pub fn new(show_all_users: bool) -> anyhow::Result<NvmlMetricsCollector> {
        let nvml = Nvml::init().with_context(|| "Nvml initialization failed")?;
        let (known_user_map, blocked_user_map) = utils::get_users_map();

        Ok(NvmlMetricsCollector {
            nvml,
            show_all_users,
            known_user_map,
            blocked_user_map,
        })
    }

    pub fn now(&mut self) -> anyhow::Result<NvmlMetrics> {
        let nvml = &self.nvml;

        let version = nvml.sys_driver_version()?;
        let device_count = nvml.device_count()?;
        let mut devices = Vec::new();
        let mut users_utilization = Vec::new();
        for index in 0..device_count {
            let device = nvml.device_by_index(index)?;
            let uuid = device.uuid()?;
            let name = device.name()?;
            let minor_number = device.minor_number()?;
            let temperature = device.temperature(TemperatureSensor::Gpu)?;
            let power_usage = device.power_usage()?;
            let fan_speed = device.fan_speed(0)?;
            let memory_info = device.memory_info()?;
            let utilization = device.utilization_rates()?;
            devices.push(NvmlDevice {
                index,
                minor_number,
                name,
                uuid,
                temperature,
                power_usage,
                fan_speed,
                memory_total: memory_info.total,
                memory_used: memory_info.used,
                utilization_memory: utilization.memory,
                utilization_gpu: utilization.gpu,
            });

            let compute_processes = device.running_compute_processes()?;
            let graphic_processes = device.running_graphics_processes()?;
            let mut user_usage: HashMap<uid_t, u64> = HashMap::new();
            for proc_info in compute_processes.iter().chain(graphic_processes.iter()) {
                let proc = procfs::process::Process::new(proc_info.pid as i32);
                let proc = if let Ok(proc) = proc {
                    proc
                } else {
                    continue;
                };
                let uid = if let Ok(uid) = proc.uid() {
                    uid
                } else {
                    continue;
                };
                // tracing::trace!("Nvml process pid = {}, uid = {}", proc.pid, uid);
                let r = match proc_info.used_gpu_memory {
                    UsedGpuMemory::Used(u) => u,
                    UsedGpuMemory::Unavailable => 0,
                };

                user_usage.entry(uid).and_modify(|e| *e += r).or_insert(r);
            }

            // for user in self.known_user_map.values().chain(self.blocked_user_map.values()) {
            //     user_usage.entry(user.uid()).or_insert(0);
            // }

            for (uid, _used) in user_usage.iter() {
                if !self.known_user_map.contains_key(uid)
                    && !self.blocked_user_map.contains_key(uid)
                {
                    let (new_known, new_blocked) = utils::get_users_map();
                    self.known_user_map = new_known;
                    self.blocked_user_map = new_blocked;
                    break;
                }
            }

            for (uid, used_gpu_memory) in user_usage.iter() {
                let user_name = if self.known_user_map.contains_key(uid) {
                    self.known_user_map[uid]
                        .name()
                        .to_string_lossy()
                        .to_string()
                } else if self.show_all_users {
                    if self.blocked_user_map.contains_key(uid) {
                        self.blocked_user_map[uid]
                            .name()
                            .to_string_lossy()
                            .to_string()
                    } else {
                        uid.to_string()
                    }
                } else {
                    continue;
                };

                let used_gpu_memory = *used_gpu_memory;
                users_utilization.push(NvmlUserUtilization {
                    index,
                    user_name,
                    used_gpu_memory,
                })
            }
        }

        Ok(NvmlMetrics {
            version,
            devices,
            users_utilization,
        })
    }
}
