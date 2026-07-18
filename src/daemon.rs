use crate::controller::Controller;
use crate::hardware::{Fan, Hardware, Sensor};
use crate::{Config, load_config};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

struct Channel {
    name: String,
    sensors: Vec<(Sensor, f32, bool)>, // sensor, setpoint, over-temperature warning active
    follow: Vec<String>,
    fan: Fan,
    primary_setpoint: f32,
    controller: Controller,
}

fn build(config: &Config, hardware: &mut Hardware) -> Result<Vec<Channel>, String> {
    let mut channels = Vec::new();

    for c in &config.channels {
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

        let mut sensors = Vec::new();
        for s in &c.sensor {
            sensors.push((hardware.resolve_sensor(&s.source)?, s.setpoint, false));
        }

        let fan = hardware.resolve_fan(&c.fan)?;
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

        // Pass 1: sensor-driven channels.
        for channel in &mut channels {
            if !channel.follow.is_empty() {
                continue;
            }

            // Max error across sensors; a failed sensor forces max fan.
            let mut error = f32::NEG_INFINITY;
            let mut failed = false;

            for (sensor, setpoint, warning_active) in &mut channel.sensors {
                match hardware.read_temperature(sensor) {
                    Ok(temperature) if temperature.is_finite() => {
                        let sensor_error = temperature - *setpoint;
                        error = error.max(sensor_error);

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
                    }
                    Ok(temperature) => {
                        eprintln!("{}: invalid sensor value: {temperature}", channel.name);
                        failed = true;
                    }
                    Err(e) => {
                        eprintln!("{}: sensor read failed: {e}", channel.name);
                        failed = true;
                    }
                }
            }

            let duty = if failed || !error.is_finite() {
                channel.controller.ceiling()
            } else {
                // Worst sensor, expressed as a temperature on the controller's
                // own setpoint scale.
                channel.controller.step(channel.primary_setpoint + error)
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
