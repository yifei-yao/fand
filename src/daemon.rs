use crate::controller::Controller;
use crate::hardware::{Fan, Hardware, Sensor};
use crate::{Config, load_config};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

struct Channel {
    name: String,
    sensor: Option<(Sensor, f32, bool)>, // sensor, setpoint, over-temperature warning active
    follow: Vec<String>,
    fan: Fan,
    controller: Controller,
}

fn build(config: &Config, hardware: &mut Hardware) -> Result<Vec<Channel>, String> {
    let mut channels = Vec::new();

    for c in &config.channels {
        for followed in &c.follow {
            if !config.channels.iter().any(|other| &other.name == followed) {
                return Err(format!(
                    "channel '{}' follows unknown channel '{followed}'",
                    c.name
                ));
            }
        }

        let sensor = match &c.sensor {
            Some(sensor) => Some((
                hardware.resolve_sensor(&sensor.source)?,
                sensor.setpoint,
                false,
            )),
            None => None,
        };

        let fan = hardware.resolve_fan(&c.fan)?;
        let setpoint = c
            .sensor
            .as_ref()
            .map(|sensor| sensor.setpoint)
            .unwrap_or(0.0);

        channels.push(Channel {
            name: c.name.clone(),
            sensor,
            follow: c.follow.clone(),
            fan,
            controller: Controller::new(setpoint, c.floor, c.ceiling),
        });
    }

    Ok(channels)
}

pub fn run(config_path: &str) -> Result<(), String> {
    let config = load_config(config_path)?;
    let mut hardware = Hardware::new();
    let mut channels = build(&config, &mut hardware)?;

    for channel in &mut channels {
        hardware.engage_fan(&mut channel.fan)?;
    }

    let running = Arc::new(AtomicBool::new(true));
    {
        let running = running.clone();
        ctrlc::set_handler(move || running.store(false, Ordering::SeqCst))
            .map_err(|e| e.to_string())?;
    }

    eprintln!("fand: controlling {} channel(s)", channels.len());

    while running.load(Ordering::SeqCst) {
        let mut duties: HashMap<String, f32> = HashMap::new();

        // Pass 1: one sensor -> one controller -> one fan channel.
        for channel in &mut channels {
            let Some((sensor, setpoint, warning_active)) = &mut channel.sensor else {
                continue;
            };

            let duty = match hardware.read_temperature(sensor) {
                Ok(temperature) if temperature.is_finite() => {
                    let sensor_error = temperature - *setpoint;

                    const WARNING_THRESHOLD_CELSIUS: f32 = 5.0;
                    if sensor_error >= WARNING_THRESHOLD_CELSIUS {
                        if !*warning_active {
                            eprintln!(
                                "WARNING: {} temperature {:.1}C is {:.1}C above target {:.1}C",
                                channel.name, temperature, sensor_error, setpoint
                            );
                            *warning_active = true;
                        }
                    } else {
                        *warning_active = false;
                    }

                    channel.controller.step(temperature)
                }
                Ok(temperature) => {
                    eprintln!("{}: invalid sensor value: {temperature}", channel.name);
                    channel.controller.ceiling()
                }
                Err(e) => {
                    eprintln!("{}: sensor read failed: {e}", channel.name);
                    channel.controller.ceiling()
                }
            };

            duties.insert(channel.name.clone(), duty);
        }

        // Pass 2: follower channels mirror the max duty of who they follow.
        for channel in &channels {
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
        for channel in &mut channels {
            let Some(&duty) = duties.get(&channel.name) else {
                continue;
            };

            if let Err(e) = hardware.set_fan_duty(&mut channel.fan, duty) {
                eprintln!("{}: fan write failed: {e}", channel.name);
            }
        }

        std::thread::sleep(Duration::from_secs_f32(config.interval_seconds));
    }

    eprintln!("fand: restoring automatic control");
    for channel in &mut channels {
        hardware.release_fan(&mut channel.fan);
    }
    hardware.release_gpus();

    Ok(())
}
