// Interactive setup: scan hwmon + NVML, show live readings, let the user
// pick fans and sensors by number, write the config.
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
    print!("{question}");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
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

pub fn interactive(config_path: &str) -> Result<(), String> {
    let mut sensors: Vec<FoundSensor> = Vec::new();
    let mut fans: Vec<FoundFan> = Vec::new();

    let (hwmon_temps, hwmon_pwms) = crate::hwmon::scan();
    for t in &hwmon_temps {
        sensors.push(FoundSensor {
            reference: format!("hwmon:{}/{}", t.chip, t.file),
            description: format!("{:<30} {:>6.1} C  ({} {})", 
                format!("{}/{}", t.chip, t.label), t.celsius, t.chip, t.file),
        });
    }
    for p in &hwmon_pwms {
        let percent = p.current_raw as f32 / 255.0 * 100.0;
        let note = if p.has_enable { "" } else { " [no pwm_enable]" };
        fans.push(FoundFan {
            reference: format!("hwmon:{}/{}", p.chip, p.file),
            description: format!("{:<30} now {:>3.0}%{note}", 
                format!("{}/{}", p.chip, p.file), percent),
        });
    }

    #[cfg(feature = "gpu")]
    {
        for index in 0..8u32 {
            match crate::gpu::Gpu::new(index) {
                Ok(g) => {
                    let temp = g.temp().unwrap_or(f32::NAN);
                    let name = g.name();
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
    }

    if sensors.is_empty() || fans.is_empty() {
        return Err("no sensors or no controllable fans found (need root?)".to_string());
    }

    println!("\nTemperature sensors:");
    for (i, s) in sensors.iter().enumerate() {
        println!("  [{i}] {}", s.description);
    }
    println!("\nControllable fans:");
    for (i, f) in fans.iter().enumerate() {
        println!("  [{i}] {}", f.description);
    }

    let mut channels: Vec<ChannelConfig> = Vec::new();
    loop {
        println!("\n--- channel {} ---", channels.len() + 1);
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

        let existing: Vec<String> = channels.iter().map(|c: &ChannelConfig| c.name.clone()).collect();
        let mut follow: Vec<String> = Vec::new();
        let mut sensor_configs = Vec::new();
        let follows_others = !existing.is_empty()
            && prompt(&format!(
                "follow other channels' duty instead of sensors (max of them)? existing: {} [y/N]: ",
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
        let picks = prompt("sensor number(s), comma-separated (case fans can watch several): ")?;
        for pick in picks.split(',') {
            let sensor_index: usize = pick
                .trim()
                .parse()
                .map_err(|_| format!("'{pick}' is not a number"))?;
            let source = sensors
                .get(sensor_index)
                .ok_or("sensor number out of range")?
                .reference
                .clone();
            let setpoint = prompt_f32(&format!("  setpoint C for {source}"), 78.0)?;
            sensor_configs.push(SensorConfig { source, setpoint });
        }
        }

        let floor = prompt_f32("floor duty % (lowest the fan spins reliably)", 25.0)?;
        let ceiling = prompt_f32("ceiling duty %", 100.0)?;

        channels.push(ChannelConfig {
            name,
            fan,
            floor,
            ceiling,
            sensor: sensor_configs,
            follow,
        });

        if prompt("add another channel? [y/N]: ")?.to_lowercase() != "y" {
            break;
        }
    }

    let config = Config {
        interval_seconds: 2.0,
        channels,
    };
    let toml_text = toml::to_string_pretty(&config).map_err(|e| e.to_string())?;
    std::fs::write(config_path, &toml_text).map_err(|e| e.to_string())?;
    println!("\nwrote {config_path}:\n\n{toml_text}");
    println!("start with: sudo fand run {config_path}");
    Ok(())
}
