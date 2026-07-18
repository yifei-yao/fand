// NVIDIA via raw NVML bindings, loaded at runtime.
// The project always compiles this module; systems without NVIDIA simply fail
// NVML discovery cleanly at runtime when libnvidia-ml.so.1 is unavailable.
use nvml_wrapper_sys::bindings::{
    NvmlLib, nvmlDevice_t, nvmlReturn_enum_NVML_SUCCESS as OK, nvmlTemperature_t,
    nvmlTemperatureSensors_enum_NVML_TEMPERATURE_GPU as TEMP_GPU,
};

pub struct Gpu {
    lib: NvmlLib,
    device: nvmlDevice_t,
    fan_count: u32,
    manual: bool,
}

fn check(code: u32, what: &str) -> Result<(), String> {
    if code == OK {
        Ok(())
    } else {
        Err(format!("NVML error {code} during {what}"))
    }
}

impl Gpu {
    pub fn new(index: u32) -> Result<Gpu, String> {
        let lib = unsafe { NvmlLib::new("libnvidia-ml.so.1") }
            .map_err(|e| format!("loading libnvidia-ml.so.1: {e}"))?;
        unsafe { check(lib.nvmlInit_v2(), "init")? };

        let mut device: nvmlDevice_t = std::ptr::null_mut();
        if let Err(error) = unsafe {
            check(
                lib.nvmlDeviceGetHandleByIndex_v2(index, &mut device),
                "get handle",
            )
        } {
            unsafe {
                let _ = lib.nvmlShutdown();
            }
            return Err(error);
        }

        let mut fan_count: u32 = 0;
        if let Err(error) =
            unsafe { check(lib.nvmlDeviceGetNumFans(device, &mut fan_count), "num fans") }
        {
            unsafe {
                let _ = lib.nvmlShutdown();
            }
            return Err(error);
        }

        Ok(Gpu {
            lib,
            device,
            fan_count,
            manual: false,
        })
    }

    pub fn name(&self) -> String {
        let mut buf = [0u8; 96];
        let code = unsafe {
            self.lib
                .nvmlDeviceGetName(self.device, buf.as_mut_ptr().cast(), buf.len() as u32)
        };
        if code == OK {
            String::from_utf8_lossy(&buf)
                .trim_end_matches('\0')
                .to_string()
        } else {
            "NVIDIA GPU".to_string()
        }
    }

    pub fn temp(&self) -> Result<f32, String> {
        // nvmlDeviceGetTemperature is deprecated as of CUDA 13.0.
        // nvmlDeviceGetTemperatureV is the current versioned API.
        const NVML_TEMPERATURE_V1: u32 = 0x0100_000C;

        let mut temperature = nvmlTemperature_t {
            version: NVML_TEMPERATURE_V1,
            sensorType: TEMP_GPU,
            temperature: 0,
        };

        let get_temperature = self
            .lib
            .nvmlDeviceGetTemperatureV
            .as_ref()
            .map_err(|e| format!("loading nvmlDeviceGetTemperatureV: {e}"))?;

        unsafe {
            check(
                get_temperature(self.device, &mut temperature),
                "get temperature",
            )?
        };

        Ok(temperature.temperature as f32)
    }

    pub fn fan_duty(&self) -> Result<f32, String> {
        if self.fan_count == 0 {
            return Err("GPU reports no fans".to_string());
        }

        let get_target_fan_speed = self
            .lib
            .nvmlDeviceGetTargetFanSpeed
            .as_ref()
            .map_err(|e| format!("loading nvmlDeviceGetTargetFanSpeed: {e}"))?;

        let mut total = 0.0f32;
        for fan in 0..self.fan_count {
            let mut target_speed: u32 = 0;
            unsafe {
                check(
                    get_target_fan_speed(self.device, fan, &mut target_speed),
                    "get target fan speed",
                )?
            };
            total += target_speed as f32;
        }

        Ok(total / self.fan_count as f32)
    }

    pub fn set_duty(&mut self, duty_percent: f32) -> Result<(), String> {
        let speed = duty_percent.clamp(0.0, 100.0).round() as u32;
        for fan in 0..self.fan_count {
            unsafe {
                check(
                    self.lib.nvmlDeviceSetFanSpeed_v2(self.device, fan, speed),
                    "set fan speed (need root?)",
                )?
            };
        }
        self.manual = true;
        Ok(())
    }

    pub fn release(&mut self) {
        if self.manual {
            for fan in 0..self.fan_count {
                unsafe {
                    let _ = self.lib.nvmlDeviceSetDefaultFanSpeed_v2(self.device, fan);
                }
            }
            self.manual = false;
        }
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        self.release();
        unsafe {
            let _ = self.lib.nvmlShutdown();
        }
    }
}
