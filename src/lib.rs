pub mod controller;
pub mod daemon;
pub mod gpu;
pub mod hardware;
pub mod hwmon;
pub mod manual;
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensor: Option<SensorConfig>,
    // Follower channel: duty = max duty of these channels, clamped to floor/ceiling.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub follow: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    #[serde(rename = "channel")]
    pub channels: Vec<ChannelConfig>,
}

pub fn load_config(path: &str) -> Result<Config, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("reading {path}: {e}"))?;
    let config: Config = toml::from_str(&raw).map_err(|e| format!("parsing {path}: {e}"))?;

    if config.channels.is_empty() {
        return Err("no [[channel]] entries; run `fand setup` first".to_string());
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
        if channel.sensor.is_some() == !channel.follow.is_empty() {
            return Err(format!(
                "channel '{}': configure exactly one sensor or follow one or more channels",
                channel.name
            ));
        }
        if let Some(sensor) = &channel.sensor {
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
