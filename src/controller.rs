use std::{fmt, time::Instant};

// Fixed internal control timing/filter parameters.
//
// X: sample the sensor this many times per second.
// P: after one second at a sustained new temperature, the EMA has closed this
//    fraction of the gap toward that temperature.
// C: run PID and update the physical fan once every C sensor samples.
pub const SENSOR_SAMPLE_RATE_HZ: f32 = 10.0;
pub const EMA_RESPONSE_PER_SECOND: f32 = 0.65;
pub const FAN_UPDATE_EVERY_SAMPLES: usize = 5;

pub struct Controller {
    setpoint: f32,
    floor: f32,
    ceiling: f32,
    duty: f32,
    integral_error: f32,
    start: Instant,
    last_call_seconds: Option<f32>,
    previous_temperature: Option<f32>,
    temp_filter: TempFilter,
}

impl Controller {
    pub fn new(setpoint: f32, floor: f32, ceiling: f32) -> Controller {
        assert!(setpoint.is_finite(), "setpoint must be finite");
        assert!(floor.is_finite(), "floor must be finite");
        assert!(ceiling.is_finite(), "ceiling must be finite");
        assert!(floor <= ceiling, "floor must not exceed ceiling");
        assert!(
            SENSOR_SAMPLE_RATE_HZ.is_finite() && SENSOR_SAMPLE_RATE_HZ > 0.0,
            "sensor sample rate must be finite and greater than zero"
        );
        assert!(
            EMA_RESPONSE_PER_SECOND.is_finite()
                && EMA_RESPONSE_PER_SECOND >= 0.0
                && EMA_RESPONSE_PER_SECOND <= 1.0,
            "EMA response per second must be within 0..=1"
        );
        assert!(
            FAN_UPDATE_EVERY_SAMPLES > 0,
            "fan update scalar must be greater than zero"
        );

        Controller {
            setpoint,
            floor,
            ceiling,
            duty: floor,

            integral_error: 0.0,

            start: Instant::now(),
            last_call_seconds: None,
            previous_temperature: None,

            temp_filter: TempFilter::new(),
        }
    }

    // Called for every raw sensor sample. The existing bad-value/jump guard is
    // applied first, then the accepted temperature feeds the infinite-memory
    // EMA. The returned value is the adjusted temperature used by the PID.
    pub fn sample_temperature(
        &mut self,
        raw_temperature: Option<f32>,
    ) -> Result<f32, InvalidTempError> {
        let now_seconds = self.start.elapsed().as_secs_f32();
        self.temp_filter.filter(raw_temperature, now_seconds)
    }

    // Called only at the actuator/control rate (once every C sensor samples).
    // P, I, and D all operate on the same EMA-adjusted temperature, and D is
    // derived from consecutive adjusted temperatures at this same cadence.
    pub fn step(&mut self, temperature: f32) -> f32 {
        const KP: f32 = 0.05;
        const KI: f32 = 0.0025;
        const KD: f32 = 0.005;
        let now_seconds = self.start.elapsed().as_secs_f32();

        let elapsed = match self.last_call_seconds {
            Some(previous) => {
                const MAX_GAP_SECONDS: f32 = 30.0;
                (now_seconds - previous).min(MAX_GAP_SECONDS)
            }
            None => 0.0,
        };

        self.last_call_seconds = Some(now_seconds);

        /*
         * Because the setpoint is constant:
         *
         *     d(error)/dt
         *         = d(temperature - setpoint)/dt
         *         = d(temperature)/dt
         *
         * The input temperature is already EMA-filtered, so derivative action
         * is simply the slope between consecutive control/fan updates.
         */
        let error_rate = match self.previous_temperature {
            Some(previous) if elapsed > f32::EPSILON => (temperature - previous) / elapsed,
            _ => 0.0,
        };
        self.previous_temperature = Some(temperature);

        /*
         * Positive error means the temperature is above the setpoint; negative
         * error means it is below the setpoint.
         *
         * Only the proportional term uses the nonlinear adjusted error. The
         * integral term integrates the real signed EMA-filtered error, and the
         * derivative term uses the EMA-filtered temperature slope.
         */
        let error = temperature - self.setpoint;
        let duty_range = self.ceiling - self.floor;

        const PROPORTIONAL_REFERENCE_ERROR_CELSIUS: f32 = 2.0;
        const PROPORTIONAL_POWER: f32 = 1.8;

        let reference_error = PROPORTIONAL_REFERENCE_ERROR_CELSIUS;

        let proportional_error = error.signum()
            * reference_error
            * (error.abs() / reference_error).powf(PROPORTIONAL_POWER);

        let proportional = duty_range * KP * proportional_error;
        let derivative = duty_range * KD * error_rate;

        /*
         * Limit the integral contribution to one complete duty range in either
         * direction. Conditional integration below provides the primary
         * anti-windup behavior; this limit is an additional safety bound.
         */
        let integral_error_limit = 1.0 / KI;

        let proposed_integral_error = (self.integral_error + error * elapsed)
            .clamp(-integral_error_limit, integral_error_limit);

        let proposed_integral = duty_range * KI * proposed_integral_error;

        let proposed_duty = self.floor + proportional + proposed_integral + derivative;

        /*
         * Conditional-integration anti-windup:
         *
         * Do not integrate when the output is already saturated and the
         * current error would push it farther into saturation.
         */
        let pushes_above_ceiling = proposed_duty > self.ceiling && error > 0.0;
        let pushes_below_floor = proposed_duty < self.floor && error < 0.0;

        if !pushes_above_ceiling && !pushes_below_floor {
            self.integral_error = proposed_integral_error;
        }

        let integral = duty_range * KI * self.integral_error;

        self.duty =
            (self.floor + proportional + integral + derivative).clamp(self.floor, self.ceiling);

        self.duty
    }

    pub fn ceiling(&self) -> f32 {
        self.ceiling
    }

    pub fn floor(&self) -> f32 {
        self.floor
    }
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InvalidTempError;

impl fmt::Display for InvalidTempError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid")
    }
}

struct TempFilter {
    last_tmp: Option<f32>,
    suspicious_since: Option<f32>,
    invalid_since: Option<f32>,
    adjusted_tmp: Option<f32>,
    ema_sample_fraction: f32,
}

impl TempFilter {
    fn new() -> Self {
        let ema_sample_fraction = if EMA_RESPONSE_PER_SECOND <= 0.0 {
            0.0
        } else if EMA_RESPONSE_PER_SECOND >= 1.0 {
            1.0
        } else {
            1.0 - (1.0 - EMA_RESPONSE_PER_SECOND).powf(1.0 / SENSOR_SAMPLE_RATE_HZ)
        };

        Self {
            last_tmp: None,
            suspicious_since: None,
            invalid_since: None,
            adjusted_tmp: None,
            ema_sample_fraction,
        }
    }

    fn filter(&mut self, raw_tmp: Option<f32>, now_seconds: f32) -> Result<f32, InvalidTempError> {
        const INVALID_HOLD_SECONDS: f32 = 3.0;
        let raw_tmp = match raw_tmp.filter(|tmp| tmp.is_finite()) {
            Some(tmp) => {
                self.invalid_since = None;
                tmp
            }
            None => {
                let since = *self.invalid_since.get_or_insert(now_seconds);
                if now_seconds - since >= INVALID_HOLD_SECONDS {
                    return Err(InvalidTempError);
                }
                self.suspicious_since = None;
                self.last_tmp.ok_or(InvalidTempError)?
            }
        };
        let accepted_tmp = self.filter_jump(raw_tmp, now_seconds);
        let adjusted_tmp = match self.adjusted_tmp {
            None => accepted_tmp,
            Some(previous) => previous + self.ema_sample_fraction * (accepted_tmp - previous),
        };
        self.adjusted_tmp = Some(adjusted_tmp);
        Ok(adjusted_tmp)
    }

    fn filter_jump(&mut self, raw_tmp: f32, now_seconds: f32) -> f32 {
        const JUMP_LIMIT_CELSIUS: f32 = 3.0;
        const JUMP_HOLD_SECONDS: f32 = 1.0;
        let Some(last_tmp) = self.last_tmp else {
            self.last_tmp = Some(raw_tmp);
            return raw_tmp;
        };
        if (raw_tmp - last_tmp).abs() < JUMP_LIMIT_CELSIUS {
            self.suspicious_since = None;
            self.last_tmp = Some(raw_tmp);
            return raw_tmp;
        }
        let Some(suspicious_since) = self.suspicious_since else {
            self.suspicious_since = Some(now_seconds);
            return last_tmp;
        };
        if now_seconds - suspicious_since < JUMP_HOLD_SECONDS {
            return last_tmp;
        }
        self.suspicious_since = None;
        self.last_tmp = Some(raw_tmp);
        raw_tmp
    }
}
