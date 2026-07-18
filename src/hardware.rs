use crate::gpu::Gpu;
use crate::hwmon;
use std::collections::HashMap;
use std::path::PathBuf;

pub enum Sensor {
    Hwmon(PathBuf),
    Nvml(u32),
}

pub enum Fan {
    Hwmon(hwmon::PwmFan),
    Nvml(u32),
}

pub struct Hardware {
    gpus: HashMap<u32, Gpu>,
}

impl Hardware {
    pub fn new() -> Self {
        Self {
            gpus: HashMap::new(),
        }
    }

    fn ensure_gpu(&mut self, index: u32) -> Result<(), String> {
        if !self.gpus.contains_key(&index) {
            self.gpus.insert(index, Gpu::new(index)?);
        }
        Ok(())
    }

    pub fn resolve_sensor(&mut self, reference: &str) -> Result<Sensor, String> {
        if let Some(reference) = reference.strip_prefix("hwmon:") {
            return Ok(Sensor::Hwmon(hwmon::resolve(reference)?));
        }

        if reference.starts_with("nvml:") {
            let index = parse_nvml_index(reference)?;
            self.ensure_gpu(index)?;
            return Ok(Sensor::Nvml(index));
        }

        Err(format!("unknown sensor source '{reference}'"))
    }

    pub fn resolve_fan(&mut self, reference: &str) -> Result<Fan, String> {
        if let Some(reference) = reference.strip_prefix("hwmon:") {
            return Ok(Fan::Hwmon(hwmon::PwmFan::new(hwmon::resolve(reference)?)));
        }

        if reference.starts_with("nvml:") {
            let index = parse_nvml_index(reference)?;
            self.ensure_gpu(index)?;
            return Ok(Fan::Nvml(index));
        }

        Err(format!("unknown fan '{reference}'"))
    }

    pub fn read_temperature(&self, sensor: &Sensor) -> Result<f32, String> {
        match sensor {
            Sensor::Hwmon(path) => hwmon::read_temp(path),
            Sensor::Nvml(index) => self
                .gpus
                .get(index)
                .ok_or_else(|| format!("NVML GPU {index} is not initialized"))?
                .temp(),
        }
    }

    pub fn read_fan_duty(&self, fan: &Fan) -> Result<f32, String> {
        match fan {
            Fan::Hwmon(fan) => fan.read_duty(),
            Fan::Nvml(index) => self
                .gpus
                .get(index)
                .ok_or_else(|| format!("NVML GPU {index} is not initialized"))?
                .fan_duty(),
        }
    }

    pub fn engage_fan(&mut self, fan: &mut Fan) -> Result<(), String> {
        match fan {
            Fan::Hwmon(fan) => fan.engage(),
            // NVML switches to manual control when set_duty() is first called.
            Fan::Nvml(_) => Ok(()),
        }
    }

    pub fn set_fan_duty(&mut self, fan: &mut Fan, duty_percent: f32) -> Result<(), String> {
        match fan {
            Fan::Hwmon(fan) => fan.set_duty(duty_percent),
            Fan::Nvml(index) => self
                .gpus
                .get_mut(index)
                .ok_or_else(|| format!("NVML GPU {index} is not initialized"))?
                .set_duty(duty_percent),
        }
    }

    pub fn release_fan(&mut self, fan: &mut Fan) {
        if let Fan::Hwmon(fan) = fan {
            fan.release();
        }
    }

    pub fn release_gpus(&mut self) {
        for gpu in self.gpus.values_mut() {
            gpu.release();
        }
    }
}

impl Default for Hardware {
    fn default() -> Self {
        Self::new()
    }
}

pub fn parse_nvml_index(reference: &str) -> Result<u32, String> {
    reference
        .strip_prefix("nvml:")
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("bad nvml reference '{reference}'"))
}
