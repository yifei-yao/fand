use std::time::Instant;

pub struct Controller {
    setpoint: f32,
    floor: f32,
    ceiling: f32,
    duty: f32,

    integral_error: f32,

    start: Instant,
    last_call_seconds: Option<f32>,

    samples: SampleWindow,
    filter: TmpFilter,
}

impl Controller {
    /*
     * The gains are expressed as fractions of the available duty range.
     *
     * For a duty range of 0..100:
     *
     *   KP = 0.15  -> 15 duty points per °C
     *   KI = 0.002 -> 0.2 duty points per °C-second
     *   KD = 0.02  -> 2 duty points per °C/second
     *
     * For a duty range of 0.0..1.0, they scale automatically.
     */
    const KP: f32 = 0.15;
    const KI: f32 = 0.002;
    const KD: f32 = 0.02;

    pub fn new(setpoint: f32, floor: f32, ceiling: f32) -> Controller {
        assert!(setpoint.is_finite(), "setpoint must be finite");
        assert!(floor.is_finite(), "floor must be finite");
        assert!(ceiling.is_finite(), "ceiling must be finite");
        assert!(floor <= ceiling, "floor must not exceed ceiling");

        Controller {
            setpoint,
            floor,
            ceiling,
            duty: floor,

            integral_error: 0.0,

            start: Instant::now(),
            last_call_seconds: None,

            samples: SampleWindow::new(),
            filter: TmpFilter::new(),
        }
    }

    pub fn step(&mut self, temperature: f32) -> f32 {
        let now_seconds = self.start.elapsed().as_secs_f32();

        let elapsed = match self.last_call_seconds {
            Some(previous) => {
                const MAX_GAP_SECONDS: f32 = 30.0;
                (now_seconds - previous).min(MAX_GAP_SECONDS)
            }
            None => 0.0,
        };

        self.last_call_seconds = Some(now_seconds);

        let Some(temperature) = self.filter.filter(temperature, elapsed) else {
            return self.duty;
        };

        self.samples.push(now_seconds, temperature);

        /*
         * Because the setpoint is constant:
         *
         *     d(error)/dt
         *         = d(temperature - setpoint)/dt
         *         = d(temperature)/dt
         *
         * The regression gives us a filtered derivative measurement.
         * Until enough samples exist, derivative action is disabled, while
         * proportional action still works immediately.
         */
        let error_rate = self.samples.slope().unwrap_or(0.0);

        /*
         * Positive error means the temperature is above the setpoint.
         * All three terms therefore increase fan duty when cooling is needed.
         */
        let error = temperature - self.setpoint;
        let duty_range = self.ceiling - self.floor;

        let proportional = duty_range * Self::KP * error;
        let derivative = duty_range * Self::KD * error_rate;

        /*
         * Limit the integral contribution to one complete duty range in either
         * direction. Conditional integration below provides the primary
         * anti-windup behavior; this limit is an additional safety bound.
         */
        let integral_error_limit = 1.0 / Self::KI;

        let proposed_integral_error = (self.integral_error + error * elapsed)
            .clamp(-integral_error_limit, integral_error_limit);

        let proposed_integral = duty_range * Self::KI * proposed_integral_error;

        let proposed_duty = self.floor + proportional + proposed_integral + derivative;

        /*
         * Conditional-integration anti-windup:
         *
         * Do not integrate when the output is already saturated and the
         * current error would push it farther into saturation.
         *
         * Integration is still allowed when it moves the output back toward
         * the usable range.
         */
        let pushes_above_ceiling = proposed_duty > self.ceiling && error > 0.0;

        let pushes_below_floor = proposed_duty < self.floor && error < 0.0;

        if !pushes_above_ceiling && !pushes_below_floor {
            self.integral_error = proposed_integral_error;
        }

        let integral = duty_range * Self::KI * self.integral_error;

        self.duty =
            (self.floor + proportional + integral + derivative).clamp(self.floor, self.ceiling);

        eprintln!(
            "[pid] temp={temperature:.2} \
     dt={elapsed:.3} \
     error={error:.2} \
     rate={error_rate:.3} \
     P={proportional:.2} \
     I={integral:.2} \
     D={derivative:.2} \
     integral_state={:.2} \
     proposed={proposed_duty:.2} \
     duty={:.2} \
     block_high={pushes_above_ceiling} \
     block_low={pushes_below_floor}",
            self.integral_error, self.duty,
        );

        self.duty
    }

    pub fn ceiling(&self) -> f32 {
        self.ceiling
    }

    pub fn floor(&self) -> f32 {
        self.floor
    }
}

struct TmpFilter {
    last_tmp: Option<f32>,
    smoothed_tmp: Option<f32>,
    ignored: usize,
}

impl TmpFilter {
    fn new() -> Self {
        Self {
            last_tmp: None,
            smoothed_tmp: None,
            ignored: 0,
        }
    }

    fn filter(&mut self, raw_tmp: f32, elapsed: f32) -> Option<f32> {
        if !raw_tmp.is_finite() {
            return self.smoothed_tmp;
        }

        const MAX_IGNORED: usize = 2;
        const DROP_LIMIT: f32 = 4.0;
        const SMOOTHING_TIME_CONSTANT_SECONDS: f32 = 2.5;

        let accepted_tmp = match self.last_tmp {
            None => {
                self.last_tmp = Some(raw_tmp);
                raw_tmp
            }

            Some(last_tmp) if raw_tmp + DROP_LIMIT < last_tmp && self.ignored < MAX_IGNORED => {
                self.ignored += 1;
                last_tmp
            }

            Some(_) => {
                self.ignored = 0;
                self.last_tmp = Some(raw_tmp);
                raw_tmp
            }
        };

        let smoothed_tmp = match self.smoothed_tmp {
            None => accepted_tmp,
            Some(previous) => {
                let alpha = 1.0 - (-elapsed / SMOOTHING_TIME_CONSTANT_SECONDS).exp();

                previous + alpha * (accepted_tmp - previous)
            }
        };

        self.smoothed_tmp = Some(smoothed_tmp);

        Some(smoothed_tmp)
    }
}

struct SampleWindow {
    seconds: [f32; SampleWindow::CAPACITY],
    temperatures: [f32; SampleWindow::CAPACITY],
    next: usize,
    count: usize,
}

impl SampleWindow {
    const CAPACITY: usize = 32;

    fn new() -> SampleWindow {
        SampleWindow {
            seconds: [0.0; SampleWindow::CAPACITY],
            temperatures: [0.0; SampleWindow::CAPACITY],
            next: 0,
            count: 0,
        }
    }

    fn push(&mut self, seconds: f32, temperature: f32) {
        self.seconds[self.next] = seconds;
        self.temperatures[self.next] = temperature;
        self.next = (self.next + 1) % SampleWindow::CAPACITY;
        self.count = (self.count + 1).min(SampleWindow::CAPACITY);
    }

    fn slope(&self) -> Option<f32> {
        const WINDOW_SECONDS: f32 = 4.0;
        const WEIGHT_TIME_CONSTANT_SECONDS: f32 = 2.0;

        let newest_index = (self.next + SampleWindow::CAPACITY - 1) % SampleWindow::CAPACITY;

        let newest = self.seconds[newest_index];

        let mut count = 0usize;
        let mut weight_sum = 0.0f32;
        let mut weighted_seconds = 0.0f32;
        let mut weighted_temperature = 0.0f32;

        for index in 0..self.count {
            let age = newest - self.seconds[index];

            if age > WINDOW_SECONDS {
                continue;
            }

            let weight = (-age / WEIGHT_TIME_CONSTANT_SECONDS).exp();
            let relative_seconds = -age;

            count += 1;
            weight_sum += weight;
            weighted_seconds += weight * relative_seconds;
            weighted_temperature += weight * self.temperatures[index];
        }

        if count < 3 || weight_sum <= f32::EPSILON {
            return None;
        }

        let seconds_mean = weighted_seconds / weight_sum;
        let temperature_mean = weighted_temperature / weight_sum;

        let mut covariance = 0.0f32;
        let mut variance = 0.0f32;

        for index in 0..self.count {
            let age = newest - self.seconds[index];

            if age > WINDOW_SECONDS {
                continue;
            }

            let weight = (-age / WEIGHT_TIME_CONSTANT_SECONDS).exp();
            let relative_seconds = -age;

            let time_offset = relative_seconds - seconds_mean;
            let temperature_offset = self.temperatures[index] - temperature_mean;

            covariance += weight * time_offset * temperature_offset;
            variance += weight * time_offset * time_offset;
        }

        if variance <= f32::EPSILON {
            return None;
        }

        Some(covariance / variance)
    }
}
