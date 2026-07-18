pub mod controller;
pub mod daemon;
pub mod gpu;
pub mod hardware;
pub mod hwmon;
pub mod monitor;
pub mod setup;

use serde::{Deserialize, Serialize};

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

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    #[serde(default = "default_interval")]
    pub interval_seconds: f32,
    #[serde(rename = "channel")]
    pub channels: Vec<ChannelConfig>,
}

fn default_interval() -> f32 {
    1.0
}

pub fn load_config(path: &str) -> Result<Config, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("reading {path}: {e}"))?;
    let config: Config = toml::from_str(&raw).map_err(|e| format!("parsing {path}: {e}"))?;

    if config.channels.is_empty() {
        return Err("no [[channel]] entries; run `fand setup` first".to_string());
    }

    if !config.interval_seconds.is_finite() || config.interval_seconds <= 0.0 {
        return Err("interval_seconds must be finite and greater than zero".to_string());
    }

    for channel in &config.channels {
        if !channel.floor.is_finite() || !channel.ceiling.is_finite() {
            return Err(format!(
                "channel '{}': floor and ceiling must be finite",
                channel.name
            ));
        }
        if channel.floor > channel.ceiling {
            return Err(format!(
                "channel '{}': floor must not exceed ceiling",
                channel.name
            ));
        }
        for sensor in &channel.sensor {
            if !sensor.setpoint.is_finite() {
                return Err(format!(
                    "channel '{}': sensor setpoint must be finite",
                    channel.name
                ));
            }
        }
    }

    Ok(config)
}
