use std::time::Instant;
pub struct Controller {
    setpoint: f32,
    floor: f32,
    ceiling: f32,
    duty: f32,
    start: Instant,
    last_call_seconds: Option<f32>,
    last_accepted_temperature: Option<f32>,
    rejected_in_a_row: u32,
    samples: SampleWindow,
}
impl Controller {
    pub fn new(setpoint: f32, floor: f32, ceiling: f32) -> Controller {
        Controller {
            setpoint,
            floor,
            ceiling,
            duty: floor,
            start: Instant::now(),
            last_call_seconds: None,
            last_accepted_temperature: None,
            rejected_in_a_row: 0,
            samples: SampleWindow::new(),
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
        if !temperature.is_finite() || elapsed <= 0.0 {
            return self.duty;
        }
        if let Some(last_accepted) = self.last_accepted_temperature {
            const MAX_PLAUSIBLE_DROP_PER_SECOND: f32 = 4.0;
            const REJECTIONS_BEFORE_ACCEPTING: u32 = 2;
            let impossible_drop =
                last_accepted - temperature > MAX_PLAUSIBLE_DROP_PER_SECOND * elapsed;
            if impossible_drop && self.rejected_in_a_row < REJECTIONS_BEFORE_ACCEPTING {
                self.rejected_in_a_row += 1;
                return self.duty;
            }
        }
        self.rejected_in_a_row = 0;
        self.last_accepted_temperature = Some(temperature);
        self.samples.push(now_seconds, temperature);
        let Some(temperature_rate) = self.samples.slope() else {
            return self.duty;
        };
        const CLOSE_SECONDS: f32 = 40.0;
        let error = temperature - self.setpoint;
        let desired_temperature_rate = -error / CLOSE_SECONDS;
        const DUTY_PER_EXCESS_DEGREE_PER_SECOND: f32 = 1.0;
        let excess_rate = temperature_rate - desired_temperature_rate;
        self.duty = (self.duty + DUTY_PER_EXCESS_DEGREE_PER_SECOND * excess_rate * elapsed)
            .clamp(self.floor, self.ceiling);
        self.duty
    }
    pub fn ceiling(&self) -> f32 {
        self.ceiling
    }
    pub fn floor(&self) -> f32 {
        self.floor
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
    const WINDOW_SECONDS: f32 = 10.0;
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
        let newest =
            self.seconds[(self.next + SampleWindow::CAPACITY - 1) % SampleWindow::CAPACITY];
        let in_window = |i: usize| newest - self.seconds[i] <= SampleWindow::WINDOW_SECONDS;
        let mut n = 0.0f32;
        let mut seconds_mean = 0.0f32;
        let mut temperature_mean = 0.0f32;
        for i in 0..self.count {
            if in_window(i) {
                n += 1.0;
                seconds_mean += self.seconds[i];
                temperature_mean += self.temperatures[i];
            }
        }
        if n < 3.0 {
            return None;
        }
        seconds_mean /= n;
        temperature_mean /= n;
        let mut covariance = 0.0f32;
        let mut variance = 0.0f32;
        for i in 0..self.count {
            if in_window(i) {
                let dt = self.seconds[i] - seconds_mean;
                covariance += dt * (self.temperatures[i] - temperature_mean);
                variance += dt * dt;
            }
        }
        if variance <= f32::EPSILON {
            return None;
        }
        Some(covariance / variance)
    }
}
