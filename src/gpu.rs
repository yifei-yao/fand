// NVIDIA via raw NVML bindings, loaded at runtime. Only built with --features gpu.
use nvml_wrapper_sys::bindings::{
    nvmlDevice_t, nvmlReturn_enum_NVML_SUCCESS as OK,
    nvmlTemperatureSensors_enum_NVML_TEMPERATURE_GPU as TEMP_GPU, NvmlLib,
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
        unsafe { check(lib.nvmlDeviceGetHandleByIndex_v2(index, &mut device), "get handle")? };
        let mut fan_count: u32 = 0;
        unsafe { check(lib.nvmlDeviceGetNumFans(device, &mut fan_count), "num fans")? };
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
        let mut t: u32 = 0;
        unsafe {
            check(
                self.lib.nvmlDeviceGetTemperature(self.device, TEMP_GPU, &mut t),
                "get temperature",
            )?
        };
        Ok(t as f32)
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
