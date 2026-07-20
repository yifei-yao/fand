// Interactive setup: scan hwmon + NVML, show live readings, let the user
// pick fans and sensors by number, then emit only the finished TOML on stdout.
use crate::{ChannelConfig, Config, SensorConfig};
use std::io::{BufRead, Write};

struct FoundSensor {
    reference: String,
    description: String,
}

struct FoundFan {
    reference: String,
    description: String,
}

fn prompt(question: &str) -> Result<String, String> {
    eprint!("{question}");
    std::io::stderr().flush().map_err(|e| e.to_string())?;

    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;

    Ok(line.trim().to_string())
}

fn prompt_f32(question: &str, default: f32) -> Result<f32, String> {
    let answer = prompt(&format!("{question} [{default}]: "))?;
    if answer.is_empty() {
        Ok(default)
    } else {
        answer
            .parse()
            .map_err(|_| format!("'{answer}' is not a number"))
    }
}

pub fn interactive() -> Result<(), String> {
    let mut sensors: Vec<FoundSensor> = Vec::new();
    let mut fans: Vec<FoundFan> = Vec::new();

    let (hwmon_temps, hwmon_pwms) = crate::hwmon::scan();
    for t in &hwmon_temps {
        sensors.push(FoundSensor {
            reference: format!("hwmon:{}/{}", t.chip, t.file),
            description: format!(
                "{:<30} {:>6.1} C  ({} {})",
                format!("{}/{}", t.chip, t.label),
                t.celsius,
                t.chip,
                t.file
            ),
        });
    }

    for p in &hwmon_pwms {
        let percent = p.current_raw as f32 / 255.0 * 100.0;
        let note = if p.has_enable { "" } else { " [no pwm_enable]" };
        fans.push(FoundFan {
            reference: format!("hwmon:{}/{}", p.chip, p.file),
            description: format!(
                "{:<30} now {:>3.0}%{note}",
                format!("{}/{}", p.chip, p.file),
                percent
            ),
        });
    }

    // NVIDIA is optional at runtime. If NVML is absent, discovery simply stops
    // and setup continues with hwmon devices only.
    for index in 0..8u32 {
        match crate::gpu::Gpu::new(index) {
            Ok(gpu) => {
                let temp = gpu.temp().unwrap_or(f32::NAN);
                let name = gpu.name();
                sensors.push(FoundSensor {
                    reference: format!("nvml:{index}"),
                    description: format!("{name:<30} {temp:>6.1} C  (GPU {index})"),
                });
                fans.push(FoundFan {
                    reference: format!("nvml:{index}"),
                    description: format!("{name:<30} (GPU {index}, all fans)"),
                });
            }
            Err(_) => break,
        }
    }

    if sensors.is_empty() || fans.is_empty() {
        return Err("no sensors or no controllable fans found (need root?)".to_string());
    }

    eprintln!("\nTemperature sensors:");
    for (i, sensor) in sensors.iter().enumerate() {
        eprintln!("  [{i}] {}", sensor.description);
    }

    eprintln!("\nControllable fans:");
    for (i, fan) in fans.iter().enumerate() {
        eprintln!("  [{i}] {}", fan.description);
    }

    let mut channels: Vec<ChannelConfig> = Vec::new();
    loop {
        eprintln!("\n--- channel {} ---", channels.len() + 1);
        let name = prompt("channel name (e.g. cpu, gpu, case): ")?;
        if name.is_empty() {
            return Err("empty name".to_string());
        }

        let fan_index: usize = prompt("fan number: ")?
            .parse()
            .map_err(|_| "not a number".to_string())?;
        let fan = fans
            .get(fan_index)
            .ok_or("fan number out of range")?
            .reference
            .clone();

        let existing: Vec<String> = channels.iter().map(|c| c.name.clone()).collect();
        let mut follow: Vec<String> = Vec::new();
        let mut sensor = None;

        let follows_others = !existing.is_empty()
            && prompt(&format!(
                "follow other channels' duty instead of a sensor (max of them)? existing: {} [y/N]: ",
                existing.join(", ")
            ))?
            .to_lowercase()
                == "y";

        if follows_others {
            let picks = prompt("channel name(s) to follow, comma-separated: ")?;
            for pick in picks.split(',') {
                let name = pick.trim().to_string();
                if !existing.contains(&name) {
                    return Err(format!("unknown channel '{name}'"));
                }
                follow.push(name);
            }
        } else {
            let sensor_index: usize = prompt("sensor number: ")?
                .parse()
                .map_err(|_| "not a number".to_string())?;
            let source = sensors
                .get(sensor_index)
                .ok_or("sensor number out of range")?
                .reference
                .clone();
            let setpoint = prompt_f32(&format!("  setpoint C for {source}"), 78.0)?;
            sensor = Some(SensorConfig { source, setpoint });
        }

        let floor = prompt_f32("floor duty % (lowest the fan spins reliably)", 25.0)?;
        let ceiling = prompt_f32("ceiling duty %", 100.0)?;

        channels.push(ChannelConfig {
            name,
            fan,
            floor,
            ceiling,
            sensor,
            follow,
        });

        if prompt("add another channel? [y/N]: ")?.to_lowercase() != "y" {
            break;
        }
    }

    let config = Config { channels };

    eprintln!("generated config.toml");
    let toml_text = toml::to_string_pretty(&config).map_err(|e| e.to_string())?;
    print!("{toml_text}");
    std::io::stdout().flush().map_err(|e| e.to_string())?;

    Ok(())
}
