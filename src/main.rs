mod controller;
#[cfg(feature = "gpu")]
mod gpu;
mod hwmon;
mod setup;

use controller::Controller;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

// Sensor/fan references:
//   "hwmon:chip/temp1_input"  "hwmon:chip/pwm2"  "nvml:0"
#[derive(Serialize, Deserialize, Clone)]
pub struct SensorConfig {
    pub source: String,
    pub setpoint: f32,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ChannelConfig {
    pub name: String,
    pub fan: String,
    pub floor: f32,
    pub ceiling: f32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sensor: Vec<SensorConfig>,
    // Follower channel: duty = max duty of these channels, clamped to floor/ceiling.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub follow: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_interval")]
    pub interval_seconds: f32,
    #[serde(rename = "channel")]
    pub channels: Vec<ChannelConfig>,
}

fn default_interval() -> f32 {
    2.0
}

enum Sensor {
    Hwmon(std::path::PathBuf),
    #[cfg(feature = "gpu")]
    Nvml(u32),
}

enum Fan {
    Hwmon(hwmon::PwmFan),
    #[cfg(feature = "gpu")]
    Nvml(u32),
}

struct Channel {
    name: String,
    sensors: Vec<(Sensor, f32)>, // (source, setpoint)
    follow: Vec<String>,         // names of channels whose max duty we mirror
    fan: Fan,
    primary_setpoint: f32,
    controller: Controller,
}

fn parse_nvml_index(reference: &str) -> Result<u32, String> {
    reference
        .strip_prefix("nvml:")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("bad nvml reference '{reference}'"))
}

fn build(
    config: &Config,
    #[cfg(feature = "gpu")] gpus: &mut HashMap<u32, gpu::Gpu>,
) -> Result<Vec<Channel>, String> {
    let mut channels = Vec::new();
    for c in &config.channels {
        let mut sensors = Vec::new();
        for s in &c.sensor {
            let sensor = if let Some(reference) = s.source.strip_prefix("hwmon:") {
                Sensor::Hwmon(hwmon::resolve(reference)?)
            } else if s.source.starts_with("nvml:") {
                #[cfg(feature = "gpu")]
                {
                    let index = parse_nvml_index(&s.source)?;
                    if !gpus.contains_key(&index) {
                        gpus.insert(index, gpu::Gpu::new(index)?);
                    }
                    Sensor::Nvml(index)
                }
                #[cfg(not(feature = "gpu"))]
                return Err(format!(
                    "channel '{}': built without --features gpu",
                    c.name
                ));
            } else {
                return Err(format!("unknown sensor source '{}'", s.source));
            };
            sensors.push((sensor, s.setpoint));
        }
        let fan = if let Some(reference) = c.fan.strip_prefix("hwmon:") {
            Fan::Hwmon(hwmon::PwmFan::new(hwmon::resolve(reference)?))
        } else if c.fan.starts_with("nvml:") {
            #[cfg(feature = "gpu")]
            {
                let index = parse_nvml_index(&c.fan)?;
                if !gpus.contains_key(&index) {
                    gpus.insert(index, gpu::Gpu::new(index)?);
                }
                Fan::Nvml(index)
            }
            #[cfg(not(feature = "gpu"))]
            return Err(format!(
                "channel '{}': built without --features gpu",
                c.name
            ));
        } else {
            return Err(format!("unknown fan '{}'", c.fan));
        };
        if !c.follow.is_empty() && !c.sensor.is_empty() {
            return Err(format!(
                "channel '{}': use sensors or follow, not both",
                c.name
            ));
        }
        for followed in &c.follow {
            if !config.channels.iter().any(|other| &other.name == followed) {
                return Err(format!(
                    "channel '{}' follows unknown channel '{followed}'",
                    c.name
                ));
            }
        }
        let primary_setpoint = c.sensor.first().map(|s| s.setpoint).unwrap_or(0.0);
        channels.push(Channel {
            name: c.name.clone(),
            sensors,
            follow: c.follow.clone(),
            fan,
            primary_setpoint,
            controller: Controller::new(primary_setpoint, c.floor, c.ceiling),
        });
    }
    Ok(channels)
}

fn run(config_path: &str) -> Result<(), String> {
    let raw =
        std::fs::read_to_string(config_path).map_err(|e| format!("reading {config_path}: {e}"))?;
    let config: Config = toml::from_str(&raw).map_err(|e| format!("parsing config: {e}"))?;
    if config.channels.is_empty() {
        return Err("no [[channel]] entries; run `fand setup` first".to_string());
    }

    #[cfg(feature = "gpu")]
    let mut gpus: HashMap<u32, gpu::Gpu> = HashMap::new();
    let mut channels = build(
        &config,
        #[cfg(feature = "gpu")]
        &mut gpus,
    )?;

    for channel in channels.iter_mut() {
        if let Fan::Hwmon(fan) = &mut channel.fan {
            fan.engage()?;
        }
    }

    let running = Arc::new(AtomicBool::new(true));
    {
        let r = running.clone();
        ctrlc::set_handler(move || r.store(false, Ordering::SeqCst)).map_err(|e| e.to_string())?;
    }

    eprintln!("fand: {} channel(s)", channels.len());
    while running.load(Ordering::SeqCst) {
        let mut duties: HashMap<String, f32> = HashMap::new();
        // Pass 1: sensor-driven channels.
        for channel in channels.iter_mut() {
            if !channel.follow.is_empty() {
                continue;
            }
            // Max error across sensors; a failed sensor forces max fan.
            let mut error = f32::NEG_INFINITY;
            let mut failed = false;
            for (sensor, setpoint) in &channel.sensors {
                let temp = match sensor {
                    Sensor::Hwmon(path) => hwmon::read_temp(path),
                    #[cfg(feature = "gpu")]
                    Sensor::Nvml(index) => gpus[index].temp(),
                };
                match temp {
                    Ok(t) => error = error.max(t - setpoint),
                    Err(e) => {
                        eprintln!("{}: sensor read failed: {e}", channel.name);
                        failed = true;
                    }
                }
            }
            let duty = if failed || !error.is_finite() {
                channel.controller.ceiling()
            } else {
                // Worst sensor, expressed as a temperature on the
                // controller's own setpoint scale.
                channel.controller.step(channel.primary_setpoint + error)
            };
            duties.insert(channel.name.clone(), duty);
        }
        // Pass 2: follower channels mirror the max duty of who they follow.
        for channel in channels.iter() {
            if channel.follow.is_empty() {
                continue;
            }
            let duty = channel
                .follow
                .iter()
                .filter_map(|name| duties.get(name))
                .fold(f32::NEG_INFINITY, |a, &b| a.max(b));
            let duty = if duty.is_finite() {
                duty
            } else {
                channel.controller.ceiling()
            };
            duties.insert(
                channel.name.clone(),
                duty.clamp(channel.controller.floor(), channel.controller.ceiling()),
            );
        }
        // Write all duties.
        for channel in channels.iter_mut() {
            let Some(&duty) = duties.get(&channel.name) else {
                continue;
            };
            let result = match &mut channel.fan {
                Fan::Hwmon(fan) => fan.set_duty(duty),
                #[cfg(feature = "gpu")]
                Fan::Nvml(index) => gpus.get_mut(index).unwrap().set_duty(duty),
            };
            if let Err(e) = result {
                eprintln!("{}: fan write failed: {e}", channel.name);
            }
        }
        std::thread::sleep(Duration::from_secs_f32(config.interval_seconds));
    }

    eprintln!("fand: restoring automatic control");
    for channel in channels.iter_mut() {
        if let Fan::Hwmon(fan) = &mut channel.fan {
            fan.release();
        }
    }
    #[cfg(feature = "gpu")]
    for g in gpus.values_mut() {
        g.release();
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("setup") => setup::interactive(args.get(2).map(String::as_str).unwrap_or("fand.toml")),
        Some("run") => run(args.get(2).map(String::as_str).unwrap_or("fand.toml")),
        _ => {
            eprintln!(
                "usage: fand setup [config.toml]   scan hardware, build config interactively"
            );
            eprintln!("       fand run   [config.toml]   run the controller");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("fand: {e}");
        std::process::exit(1);
    }
}
