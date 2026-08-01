# fand

A simple Linux fan controller using hwmon and NVIDIA NVML.

## Usage

```text
fand setup > config.toml
fand test
fand run config.toml
fand watch [config.toml]
```

setup creates a config, test manually tests fans, run controls fans automatically, and watch shows live status.

## Limitations

Linux only. Hardware support depends on available hwmon/NVML interfaces, and fan control may require root.

- fand uses a very basic PID controller implementation and is not guaranteed to work well on every system. Use at your own risk; the authors are not liable for hardware damage, overheating, data loss, or other damages resulting from its use.
