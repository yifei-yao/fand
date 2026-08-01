use crate::hardware::{Fan, Hardware, Sensor};
use crate::{Config, load_config};
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::MissedTickBehavior;

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

fn render_line(
    channels: &[MonitorChannel],
    hardware: &Hardware,
    stderr: &mut impl Write,
) -> Result<(), String> {
    let line = channels
        .iter()
        .map(|channel| render_channel(channel, hardware))
        .collect::<Vec<_>>()
        .join(" | ");
    write!(stderr, "\r\x1b[2K{line}").map_err(|e| e.to_string())?;
    stderr.flush().map_err(|e| e.to_string())
}

pub fn run(config_path: &str) -> Result<(), String> {
    let config = load_config(config_path)?;
    let mut hardware = Hardware::new();
    let channels = build(&config, &mut hardware)?;
    let stop = Arc::new(Notify::new());
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || stop.notify_one()).map_err(|e| e.to_string())?;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|e| format!("creating Tokio runtime: {e}"))?;
    let stderr = std::io::stderr();
    let mut stderr = stderr.lock();
    let result = runtime.block_on(async {
        let period = Duration::from_secs(1);
        let mut ticker = tokio::time::interval(period);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = ticker.tick() => render_line(&channels, &hardware, &mut stderr)?,
                _ = stop.notified() => return Ok(()),
            }
        }
    });
    writeln!(stderr).map_err(|e| e.to_string())?;
    result
}
