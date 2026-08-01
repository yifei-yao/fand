mod controller;
mod daemon;
mod gpu;
mod hardware;
mod hwmon;
mod manual;
mod monitor;
mod setup;

use serde::{Deserialize, Serialize};

const DEFAULT_CONFIG_PATH: &str = "/etc/fand/config.toml";

// Sensor/fan references:
//   "hwmon:chip/temp1_input"  "hwmon:chip/pwm2"  "nvml:0"
#[derive(Serialize, Deserialize, Clone)]
struct SensorConfig {
    source: String,
    setpoint: f32,
}

#[derive(Serialize, Deserialize, Clone)]
struct ChannelConfig {
    name: String,
    fan: String,
    floor: f32,
    ceiling: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sensor: Option<SensorConfig>,
    // Follower channel: duty = max duty of these channels, clamped to floor/ceiling.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    follow: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Config {
    #[serde(rename = "channel")]
    channels: Vec<ChannelConfig>,
}

fn load_config(path: &str) -> Result<Config, String> {
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

fn print_usage() {
    eprintln!("usage: fand setup > config.toml");
    eprintln!("       fand run <config.toml>");
    eprintln!("       fand watch [config.toml]");
    eprintln!("       fand test");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let result = match args.as_slice() {
        [command] if command.as_str() == "setup" => setup::interactive(),
        [command, config_path] if command.as_str() == "run" => daemon::run(config_path),
        [command] if command.as_str() == "watch" => monitor::run(DEFAULT_CONFIG_PATH),
        [command, config_path] if command.as_str() == "watch" => monitor::run(config_path),
        [command] if command.as_str() == "test" => manual::interactive(),
        _ => {
            print_usage();
            std::process::exit(2);
        }
    };

    if let Err(error) = result {
        eprintln!("fand: {error}");
        std::process::exit(1);
    }
}
