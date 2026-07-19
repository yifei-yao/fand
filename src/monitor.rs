use crate::controller::SENSOR_SAMPLE_RATE_HZ;
use crate::hardware::{Fan, Hardware, Sensor};
use crate::{Config, load_config};
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

struct MonitorChannel {
    name: String,
    sensor: Option<(Sensor, f32)>,
    fan: Fan,
}

fn build(config: &Config, hardware: &mut Hardware) -> Result<Vec<MonitorChannel>, String> {
    let mut channels = Vec::new();

    for channel in &config.channels {
        let sensor = match &channel.sensor {
            Some(sensor) => Some((hardware.resolve_sensor(&sensor.source)?, sensor.setpoint)),
            None => None,
        };

        channels.push(MonitorChannel {
            name: channel.name.clone(),
            sensor,
            fan: hardware.resolve_fan(&channel.fan)?,
        });
    }

    Ok(channels)
}

fn render_channel(channel: &MonitorChannel, hardware: &Hardware) -> String {
    let mut text = channel.name.clone();

    if let Some((sensor, setpoint)) = &channel.sensor {
        match hardware.read_temperature(sensor) {
            Ok(temperature) if temperature.is_finite() => {
                text.push_str(&format!(" {temperature:.1}/{setpoint:.1}C"));
            }
            Ok(_) => text.push_str(" invalid"),
            Err(_) => text.push_str(" ERR"),
        }
    }

    match hardware.read_fan_duty(&channel.fan) {
        Ok(duty) if duty.is_finite() => text.push_str(&format!(" {duty:.1}%")),
        Ok(_) => text.push_str(" invalid"),
        Err(_) => text.push_str(" ERR"),
    }

    text
}

pub fn run(config_path: &str) -> Result<(), String> {
    let config = load_config(config_path)?;
    let mut hardware = Hardware::new();
    let channels = build(&config, &mut hardware)?;

    let running = Arc::new(AtomicBool::new(true));
    {
        let running = running.clone();
        ctrlc::set_handler(move || running.store(false, Ordering::SeqCst))
            .map_err(|e| e.to_string())?;
    }

    let stderr = std::io::stderr();
    let mut stderr = stderr.lock();

    while running.load(Ordering::SeqCst) {
        let line = channels
            .iter()
            .map(|channel| render_channel(channel, &hardware))
            .collect::<Vec<_>>()
            .join(" | ");

        write!(stderr, "\r\x1b[2K{line}").map_err(|e| e.to_string())?;
        stderr.flush().map_err(|e| e.to_string())?;

        std::thread::sleep(Duration::from_secs_f32(1.0 / SENSOR_SAMPLE_RATE_HZ));
    }

    writeln!(stderr).map_err(|e| e.to_string())?;
    Ok(())
}
