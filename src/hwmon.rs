// hwmon discovery and access. Config references are "chip_name/file"
// (e.g. "k10temp/temp1_input", "nct6775/pwm2") resolved to real paths at
// startup, because hwmonN indices shuffle across reboots.
use std::fs;
use std::path::PathBuf;

pub struct TempEntry {
    pub chip: String,
    pub file: String,
    pub label: String,
    pub path: PathBuf,
    pub celsius: f32,
}

pub struct PwmEntry {
    pub chip: String,
    pub file: String,
    pub path: PathBuf,
    pub current_raw: u32,
    pub has_enable: bool,
}

pub fn scan() -> (Vec<TempEntry>, Vec<PwmEntry>) {
    let mut temps = Vec::new();
    let mut pwms = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/hwmon") else {
        return (temps, pwms);
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let chip = fs::read_to_string(dir.join("name"))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        let Ok(files) = fs::read_dir(&dir) else {
            continue;
        };
        for f in files.flatten() {
            let file = f.file_name().to_string_lossy().to_string();
            if file.starts_with("temp") && file.ends_with("_input") {
                let Ok(raw) = fs::read_to_string(f.path()) else {
                    continue;
                };
                let Ok(milli) = raw.trim().parse::<f32>() else {
                    continue;
                };
                let label_file = file.replace("_input", "_label");
                let label = fs::read_to_string(dir.join(&label_file))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|_| file.clone());
                temps.push(TempEntry {
                    chip: chip.clone(),
                    file,
                    label,
                    path: f.path(),
                    celsius: milli / 1000.0,
                });
            } else if file.starts_with("pwm")
                && file.len() > 3
                && file[3..].chars().all(|c| c.is_ascii_digit())
            {
                let Ok(raw) = fs::read_to_string(f.path()) else {
                    continue;
                };
                let Ok(current_raw) = raw.trim().parse::<u32>() else {
                    continue;
                };
                let has_enable = dir.join(format!("{file}_enable")).exists();
                pwms.push(PwmEntry {
                    chip: chip.clone(),
                    file,
                    path: f.path(),
                    current_raw,
                    has_enable,
                });
            }
        }
    }
    temps.sort_by(|a, b| (&a.chip, &a.file).cmp(&(&b.chip, &b.file)));
    pwms.sort_by(|a, b| (&a.chip, &a.file).cmp(&(&b.chip, &b.file)));
    (temps, pwms)
}

pub fn resolve(reference: &str) -> Result<PathBuf, String> {
    let (chip, file) = reference
        .split_once('/')
        .ok_or_else(|| format!("bad hwmon reference '{reference}', want chip/file"))?;
    let entries = fs::read_dir("/sys/class/hwmon").map_err(|e| format!("/sys/class/hwmon: {e}"))?;
    for entry in entries.flatten() {
        let dir = entry.path();
        let name = fs::read_to_string(dir.join("name"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if name == chip && dir.join(file).exists() {
            return Ok(dir.join(file));
        }
    }
    Err(format!("hwmon chip '{chip}' with file '{file}' not found"))
}

pub fn read_temp(path: &PathBuf) -> Result<f32, String> {
    let raw = fs::read_to_string(path).map_err(|e| e.to_string())?;
    Ok(raw.trim().parse::<f32>().map_err(|e| e.to_string())? / 1000.0)
}

pub struct PwmFan {
    pwm_path: PathBuf,
    enable_path: PathBuf,
    saved_enable: Option<String>,
}

impl PwmFan {
    pub fn new(pwm_path: PathBuf) -> PwmFan {
        let enable_path = PathBuf::from(format!("{}_enable", pwm_path.display()));
        PwmFan {
            pwm_path,
            enable_path,
            saved_enable: None,
        }
    }

    pub fn read_duty(&self) -> Result<f32, String> {
        let raw = fs::read_to_string(&self.pwm_path).map_err(|e| e.to_string())?;
        let raw = raw.trim().parse::<f32>().map_err(|e| e.to_string())?;
        Ok(raw / 255.0 * 100.0)
    }

    pub fn read_rpm(&self) -> Result<Option<u32>, String> {
        let Some(file) = self.pwm_path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        let Some(index) = file.strip_prefix("pwm") else {
            return Ok(None);
        };
        if index.is_empty() || !index.chars().all(|c| c.is_ascii_digit()) {
            return Ok(None);
        }

        let rpm_path = self.pwm_path.with_file_name(format!("fan{index}_input"));
        if !rpm_path.exists() {
            return Ok(None);
        }

        let raw =
            fs::read_to_string(&rpm_path).map_err(|e| format!("{}: {e}", rpm_path.display()))?;
        let rpm = raw
            .trim()
            .parse::<u32>()
            .map_err(|e| format!("{}: {e}", rpm_path.display()))?;
        Ok(Some(rpm))
    }

    pub fn engage(&mut self) -> Result<(), String> {
        if self.enable_path.exists() {
            let previous = fs::read_to_string(&self.enable_path).map_err(|e| e.to_string())?;
            fs::write(&self.enable_path, "1")
                .map_err(|e| format!("{} (need root?): {e}", self.enable_path.display()))?;
            self.saved_enable = Some(previous.trim().to_string());
        }
        Ok(())
    }

    pub fn set_duty(&self, duty_percent: f32) -> Result<(), String> {
        let raw = ((duty_percent.clamp(0.0, 100.0) / 100.0) * 255.0).round() as u32;
        fs::write(&self.pwm_path, raw.to_string()).map_err(|e| e.to_string())
    }

    pub fn release(&mut self) {
        if let Some(previous) = self.saved_enable.take() {
            let _ = fs::write(&self.enable_path, previous);
        }
    }
}

impl Drop for PwmFan {
    fn drop(&mut self) {
        self.release();
    }
}
