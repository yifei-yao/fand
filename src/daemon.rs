use crate::controller::{Controller, FAN_UPDATE_EVERY_SAMPLES, SENSOR_SAMPLE_RATE_HZ};
use crate::hardware::Hardware;
use crate::{Config, load_config};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::{broadcast, watch};
use tokio::time::MissedTickBehavior;

struct Channel {
    name: String,
    sensor: Option<(String, f32)>,
    follow: Vec<String>,
    fan: String,
    floor: f32,
    ceiling: f32,
    duty_tx: Option<watch::Sender<f32>>,
    subscriptions: Vec<watch::Receiver<f32>>,
}

fn build(config: &Config) -> Result<Vec<Channel>, String> {
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
        channels.push(Channel {
            name: c.name.clone(),
            sensor: c
                .sensor
                .as_ref()
                .map(|sensor| (sensor.source.clone(), sensor.setpoint)),
            follow: c.follow.clone(),
            fan: c.fan.clone(),
            floor: c.floor,
            ceiling: c.ceiling,
            duty_tx: None,
            subscriptions: Vec::new(),
        });
    }
    Ok(channels)
}

fn wire_followers(channels: &mut [Channel]) -> Result<(), String> {
    for follower_index in 0..channels.len() {
        let followed_names = channels[follower_index].follow.clone();
        let mut subscriptions = Vec::with_capacity(followed_names.len());
        for followed_name in followed_names {
            let source_index = channels
                .iter()
                .position(|channel| channel.name == followed_name)
                .ok_or_else(|| {
                    format!(
                        "channel '{}' follows unknown channel '{}'",
                        channels[follower_index].name, followed_name
                    )
                })?;
            // Lazily create exactly one watch channel for this source,
            // only if something actually follows it.
            if channels[source_index].duty_tx.is_none() {
                let (duty_tx, _) = watch::channel(0.0_f32);
                channels[source_index].duty_tx = Some(duty_tx);
            }
            subscriptions.push(channels[source_index].duty_tx.as_ref().unwrap().subscribe());
        }
        channels[follower_index].subscriptions = subscriptions;
    }
    Ok(())
}

fn wait_for_tick(tick_rx: &mut broadcast::Receiver<()>) -> bool {
    loop {
        match tick_rx.blocking_recv() {
            Ok(()) => return true,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return false,
        }
    }
}

fn write_duty(
    name: &str,
    hardware: &mut Hardware,
    fan: &mut crate::hardware::Fan,
    duty_tx: &Option<watch::Sender<f32>>,
    duty: f32,
) {
    if let Err(e) = hardware.set_fan_duty(fan, duty) {
        eprintln!("{}: fan write failed: {e}", name);
    }
    if let Some(duty_tx) = duty_tx {
        duty_tx.send_replace(duty);
    }
}

fn run_sensor_channel(
    channel: Channel,
    mut tick_rx: broadcast::Receiver<()>,
    running: Arc<AtomicBool>,
) -> Result<(), String> {
    const WARNING_THRESHOLD_CELSIUS: f32 = 5.0;

    let Channel {
        name,
        sensor,
        fan,
        floor,
        ceiling,
        duty_tx,
        ..
    } = channel;

    let (sensor_source, setpoint) = sensor.expect("sensor worker without sensor");
    let mut hardware = Hardware::new();
    let sensor = hardware.resolve_sensor(&sensor_source)?;
    let mut fan = hardware.resolve_fan(&fan)?;
    hardware.engage_fan(&mut fan)?;

    let mut controller = Controller::new(setpoint, floor, ceiling);
    let mut warning_active = false;
    let mut samples_since_update = 0usize;

    while running.load(Ordering::SeqCst) && wait_for_tick(&mut tick_rx) {
        if !running.load(Ordering::SeqCst) {
            break;
        }
        let raw_temperature = hardware.read_temperature(&sensor).ok();
        let adjusted_temperature = match controller.sample_temperature(raw_temperature) {
            Ok(temperature) => temperature,
            Err(error) => {
                eprintln!("ERROR: {name}: {error}; disabling channel and releasing fan control");
                hardware.release_fan(&mut fan);
                hardware.release_gpus();
                return Ok(());
            }
        };
        let sensor_error = adjusted_temperature - setpoint;
        if sensor_error >= WARNING_THRESHOLD_CELSIUS {
            if !warning_active {
                eprintln!(
                    "WARNING: {} temperature {:.1}C is {:.1}C above target {:.1}C",
                    name, adjusted_temperature, sensor_error, setpoint
                );
                warning_active = true;
            }
        } else {
            warning_active = false;
        }
        samples_since_update += 1;
        if samples_since_update < FAN_UPDATE_EVERY_SAMPLES {
            continue;
        }
        samples_since_update = 0;
        let duty = controller.step(adjusted_temperature);
        write_duty(&name, &mut hardware, &mut fan, &duty_tx, duty);
    }

    hardware.release_fan(&mut fan);
    hardware.release_gpus();
    Ok(())
}

fn run_follower_channel(channel: Channel, runtime: Handle) -> Result<(), String> {
    let Channel {
        name,
        fan,
        floor,
        ceiling,
        duty_tx,
        mut subscriptions,
        ..
    } = channel;
    let mut hardware = Hardware::new();
    let mut fan = hardware.resolve_fan(&fan)?;
    hardware.engage_fan(&mut fan)?;
    let controller = Controller::new(0.0, floor, ceiling);
    let mut last_written = None;
    loop {
        for rx in &mut subscriptions {
            if runtime.block_on(rx.changed()).is_err() {
                hardware.release_fan(&mut fan);
                hardware.release_gpus();
                return Ok(());
            }
        }
        let duty = {
            let values: Vec<_> = subscriptions
                .iter_mut()
                .map(|rx| rx.borrow_and_update())
                .collect();
            values
                .iter()
                .map(|value| **value)
                .fold(f32::NEG_INFINITY, f32::max)
                .clamp(controller.floor(), controller.ceiling())
        };
        if last_written != Some(duty) {
            if let Err(e) = hardware.set_fan_duty(&mut fan, duty) {
                eprintln!("{}: fan write failed: {e}", name);
            } else {
                last_written = Some(duty);
            }
        }
        if let Some(duty_tx) = &duty_tx {
            duty_tx.send_replace(duty);
        }
    }
}

async fn run_async(channels: Vec<Channel>, running: Arc<AtomicBool>) -> Result<(), String> {
    let (tick_tx, _) = broadcast::channel::<()>(1);
    let mut workers = Vec::new();
    for channel in channels {
        if channel.sensor.is_some() {
            let tick_rx = tick_tx.subscribe();
            let running = running.clone();
            let stop_on_error = running.clone();
            workers.push(tokio::task::spawn_blocking(move || {
                let result = run_sensor_channel(channel, tick_rx, running);
                if result.is_err() {
                    stop_on_error.store(false, Ordering::SeqCst);
                }
                result
            }));
        } else {
            let runtime = Handle::current();
            let stop_on_error = running.clone();
            workers.push(tokio::task::spawn_blocking(move || {
                let result = run_follower_channel(channel, runtime);
                if result.is_err() {
                    stop_on_error.store(false, Ordering::SeqCst);
                }
                result
            }));
        }
    }

    let period = Duration::from_secs_f32(1.0 / SENSOR_SAMPLE_RATE_HZ);
    let mut ticker = tokio::time::interval(period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    while running.load(Ordering::SeqCst) {
        ticker.tick().await;
        if !running.load(Ordering::SeqCst) {
            break;
        }
        let _ = tick_tx.send(());
    }
    drop(tick_tx);

    for worker in workers {
        worker
            .await
            .map_err(|e| format!("channel worker failed: {e}"))??;
    }
    Ok(())
}

pub fn run(config_path: &str) -> Result<(), String> {
    let config = load_config(config_path)?;
    let mut channels = build(&config)?;
    wire_followers(&mut channels)?;
    let running = Arc::new(AtomicBool::new(true));
    {
        let running = running.clone();
        ctrlc::set_handler(move || running.store(false, Ordering::SeqCst))
            .map_err(|e| e.to_string())?;
    }
    eprintln!("fand: controlling {} channel(s)", channels.len());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .build()
        .map_err(|e| format!("creating Tokio runtime: {e}"))?;
    let result = runtime.block_on(run_async(channels, running));
    eprintln!("fand: automatic control restored");
    result
}
