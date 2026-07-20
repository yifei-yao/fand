use crate::hardware::{Fan, Hardware};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, Read, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

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

fn discover_fans() -> Vec<FoundFan> {
    let mut fans = Vec::new();

    let (_, hwmon_pwms) = crate::hwmon::scan();
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

    // NVIDIA is optional. Stop at the first missing index, matching setup.
    for index in 0..8u32 {
        match crate::gpu::Gpu::new(index) {
            Ok(gpu) => {
                let name = gpu.name();
                let duty = gpu.fan_duty().ok();
                let status = duty
                    .map(|value| format!("now {value:>3.0}%"))
                    .unwrap_or_else(|| "duty unavailable".to_string());
                fans.push(FoundFan {
                    reference: format!("nvml:{index}"),
                    description: format!("{name:<30} {status}  (GPU {index}, all fans)"),
                });
            }
            Err(_) => break,
        }
    }

    fans
}

struct RawTerminal {
    saved_state: String,
}

impl RawTerminal {
    fn enter() -> Result<(Self, File), String> {
        let tty_for_state = File::open("/dev/tty").map_err(|e| format!("opening /dev/tty: {e}"))?;
        let output = Command::new("stty")
            .arg("-g")
            .stdin(Stdio::from(tty_for_state))
            .output()
            .map_err(|e| format!("running stty -g: {e}"))?;
        if !output.status.success() {
            return Err("stty -g failed".to_string());
        }
        let saved_state = String::from_utf8(output.stdout)
            .map_err(|e| format!("reading stty state: {e}"))?
            .trim()
            .to_string();

        // Non-canonical and no echo lets us refresh the same row while the user
        // types. Keep ISIG enabled so Ctrl-C still reaches the signal handler.
        run_stty(&["-icanon", "-echo", "min", "0", "time", "1"])?;

        // Read AND write the actual controlling terminal. Do not redraw through
        // stderr: stderr may be piped/captured, in which case \r/ANSI escapes do
        // not move the user's cursor and every refresh gets appended instead.
        let tty = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .map_err(|e| format!("opening /dev/tty: {e}"))?;

        Ok((Self { saved_state }, tty))
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        let _ = run_stty(&[&self.saved_state]);
    }
}

fn run_stty(args: &[&str]) -> Result<(), String> {
    let tty = File::open("/dev/tty").map_err(|e| format!("opening /dev/tty: {e}"))?;
    let status = Command::new("stty")
        .args(args)
        .stdin(Stdio::from(tty))
        .status()
        .map_err(|e| format!("running stty: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("stty failed".to_string())
    }
}

fn terminal_columns() -> usize {
    let Ok(tty) = File::open("/dev/tty") else {
        return 80;
    };
    let Ok(output) = Command::new("stty")
        .arg("size")
        .stdin(Stdio::from(tty))
        .output()
    else {
        return 80;
    };
    let Ok(text) = String::from_utf8(output.stdout) else {
        return 80;
    };
    text.split_whitespace()
        .nth(1)
        .and_then(|columns| columns.parse::<usize>().ok())
        .filter(|columns| *columns >= 20)
        .unwrap_or(80)
}

fn fit_to_terminal(mut text: String, columns: usize) -> String {
    // Leave one column unused so terminals that auto-wrap in the final column
    // never turn the live status into a second physical row.
    let limit = columns.saturating_sub(1).max(1);
    if text.chars().count() <= limit {
        return text;
    }

    let mut shortened = String::with_capacity(limit);
    for ch in text.chars().take(limit.saturating_sub(1)) {
        shortened.push(ch);
    }
    shortened.push('…');
    text = shortened;
    text
}

fn redraw(tty: &mut File, text: String, columns: usize) -> Result<(), String> {
    let text = fit_to_terminal(text, columns);
    let clear_width = columns.saturating_sub(1).max(1);

    // Clear and repaint one physical terminal row without relying on ANSI erase
    // sequences. Because neither the spaces nor the text reaches the final
    // terminal column, automatic wrapping cannot create extra rows.
    write!(tty, "\r{:clear_width$}\r{text}", "").map_err(|e| e.to_string())?;
    tty.flush().map_err(|e| e.to_string())
}

fn clear_live_line(tty: &mut File, columns: usize) -> Result<(), String> {
    let clear_width = columns.saturating_sub(1).max(1);
    write!(tty, "\r{:clear_width$}\r", "").map_err(|e| e.to_string())?;
    tty.flush().map_err(|e| e.to_string())
}

fn restore_fan(hardware: &mut Hardware, fan: &mut Fan, original_duty: Option<f32>) {
    if let Some(duty) = original_duty {
        let _ = hardware.set_fan_duty(fan, duty);
    }
    hardware.release_fan(fan);
    hardware.release_gpus();
}

fn fan_duty_loop(fan_index: usize, found: &FoundFan, running: &AtomicBool) -> Result<(), String> {
    let mut hardware = Hardware::new();
    let mut fan = hardware.resolve_fan(&found.reference)?;
    let original_duty = hardware
        .read_fan_duty(&fan)
        .ok()
        .filter(|duty| duty.is_finite());

    if let Err(error) = hardware.engage_fan(&mut fan) {
        restore_fan(&mut hardware, &mut fan, original_duty);
        return Err(error);
    }

    // Selecting a fan freezes it at the duty we just observed. After this point
    // set_fan_duty() is called ONLY when a complete, valid number is submitted
    // with Enter. Partially typed input can never change the fan.
    if let Some(duty) = original_duty {
        if let Err(error) = hardware.set_fan_duty(&mut fan, duty) {
            restore_fan(&mut hardware, &mut fan, original_duty);
            return Err(error);
        }
    }

    let loop_result = (|| -> Result<(), String> {
        let (_raw_terminal, mut tty) = RawTerminal::enter()?;
        let columns = terminal_columns();
        let mut key = [0u8; 1];
        let mut input = String::new();
        let mut requested_duty = original_duty;
        let mut message: Option<String> = None;

        // Printed once; only the compact status/prompt below it is redrawn.
        writeln!(tty, "Enter 0-100 to apply; blank Enter or q goes back.")
            .map_err(|e| e.to_string())?;
        tty.flush().map_err(|e| e.to_string())?;

        while running.load(Ordering::SeqCst) {
            let duty_text = match hardware.read_fan_duty(&fan) {
                Ok(duty) if duty.is_finite() => format!("{duty:.1}%"),
                _ => "ERR".to_string(),
            };
            let fixed_text = requested_duty
                .map(|duty| format!("{duty:.1}%"))
                .unwrap_or_else(|| "---".to_string());
            let rpm_text = match hardware.read_fan_rpm(&fan) {
                Ok(Some(rpm)) => format!(" | {rpm} RPM"),
                Ok(None) => String::new(),
                Err(_) => " | RPM ERR".to_string(),
            };
            let message_text = message
                .as_ref()
                .map(|message| format!(" | {message}"))
                .unwrap_or_default();

            redraw(
                &mut tty,
                format!(
                    "fan[{fan_index}] set {fixed_text} | actual {duty_text}{rpm_text} | duty> {input}{message_text}"
                ),
                columns,
            )?;

            match tty.read(&mut key) {
                Ok(0) => {}
                Ok(_) => match key[0] {
                    b'0'..=b'9' => {
                        if input.len() < 6 {
                            input.push(key[0] as char);
                        }
                        message = None;
                    }
                    b'.' => {
                        if !input.contains('.') && input.len() < 6 {
                            input.push('.');
                        }
                        message = None;
                    }
                    8 | 127 => {
                        input.pop();
                        message = None;
                    }
                    b'q' | b'Q' if input.is_empty() => break,
                    b'\r' | b'\n' => {
                        if input.is_empty() {
                            break;
                        }

                        let parsed = input.parse::<f32>();
                        input.clear();

                        match parsed {
                            Ok(duty) if duty.is_finite() && (0.0..=100.0).contains(&duty) => {
                                // The one and only place the inner loop changes duty.
                                match hardware.set_fan_duty(&mut fan, duty) {
                                    Ok(()) => {
                                        requested_duty = Some(duty);
                                        message = None;
                                    }
                                    Err(error) => {
                                        message = Some(format!("write failed: {error}"));
                                    }
                                }
                            }
                            _ => {
                                message = Some("invalid: use 0-100".to_string());
                            }
                        }
                    }
                    _ => {}
                },
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(format!("reading key: {e}")),
            }

            std::thread::sleep(Duration::from_millis(100));
        }

        clear_live_line(&mut tty, columns)?;
        Ok(())
    })();

    restore_fan(&mut hardware, &mut fan, original_duty);
    loop_result
}

pub fn interactive() -> Result<(), String> {
    let fans = discover_fans();
    if fans.is_empty() {
        return Err("no controllable fans found (need root?)".to_string());
    }

    eprintln!("\nControllable fans:");
    for (i, fan) in fans.iter().enumerate() {
        eprintln!("  [{i}] {}", fan.description);
    }

    let running = Arc::new(AtomicBool::new(true));
    {
        let running = running.clone();
        ctrlc::set_handler(move || running.store(false, Ordering::SeqCst))
            .map_err(|e| e.to_string())?;
    }

    // Outer loop selects a fan. Inner loop keeps that one fan engaged and accepts
    // as many new duty values as desired before restoring it and coming back here.
    while running.load(Ordering::SeqCst) {
        let answer = prompt("\nfan number (q to quit): ")?;
        if !running.load(Ordering::SeqCst) {
            break;
        }
        if answer.eq_ignore_ascii_case("q") || answer.eq_ignore_ascii_case("quit") {
            break;
        }

        let fan_index: usize = match answer.parse() {
            Ok(index) => index,
            Err(_) => {
                eprintln!("not a fan number");
                continue;
            }
        };
        let Some(fan) = fans.get(fan_index) else {
            eprintln!("fan number out of range");
            continue;
        };

        if let Err(error) = fan_duty_loop(fan_index, fan, &running) {
            eprintln!("fan [{fan_index}]: {error}");
        }
    }

    eprintln!("fand: fan control restored");
    Ok(())
}
