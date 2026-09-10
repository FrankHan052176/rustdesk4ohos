use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

static ENABLED: OnceLock<bool> = OnceLock::new();
static NEXT_CONNECTION_ORDINAL: AtomicU64 = AtomicU64::new(1);

pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        cfg!(target_os = "windows")
            && std::env::var_os("RUSTDESK_SENDER_TRACE").as_deref()
                == Some(std::ffi::OsStr::new("1"))
    })
}

pub fn next_connection_ordinal() -> u64 {
    NEXT_CONNECTION_ORDINAL.fetch_add(1, Ordering::Relaxed)
}

pub fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub fn build_label() -> &'static str {
    option_env!("RUSTDESK_DIAGNOSTIC_BUILD").unwrap_or("local-unattributed")
}

#[derive(Default)]
struct DurationStat {
    total: Duration,
    max: Duration,
}

impl DurationStat {
    fn add(&mut self, value: Duration) {
        self.total += value;
        self.max = self.max.max(value);
    }
}

pub enum CaptureOutcome {
    Ok,
    WouldBlock,
    Error,
}

pub struct ServiceTelemetry {
    started: Instant,
    qos_latest: u32,
    qos_min: u32,
    qos_max: u32,
    spf: Duration,
    capture_calls: u64,
    capture_ok: u64,
    capture_would_block: u64,
    capture_errors: u64,
    capture_time: DurationStat,
    encode_calls: u64,
    encoded_frames: u64,
    encode_errors: u64,
    encode_time: DurationStat,
    dispatch_attempt_messages: u64,
    dispatch_attempt_frames: u64,
    payload_bytes: u64,
    fetched_wait: DurationStat,
    sleep: DurationStat,
}

pub struct ServiceSnapshot {
    pub elapsed: Duration,
    pub qos_latest: u32,
    pub qos_min: u32,
    pub qos_max: u32,
    pub spf: Duration,
    pub capture_calls: u64,
    pub capture_ok: u64,
    pub capture_would_block: u64,
    pub capture_errors: u64,
    pub capture_total: Duration,
    pub capture_max: Duration,
    pub encode_calls: u64,
    pub encoded_frames: u64,
    pub encode_errors: u64,
    pub encode_total: Duration,
    pub encode_max: Duration,
    pub dispatch_attempt_messages: u64,
    pub dispatch_attempt_frames: u64,
    pub payload_bytes: u64,
    pub fetched_wait_total: Duration,
    pub fetched_wait_max: Duration,
    pub sleep_total: Duration,
    pub sleep_max: Duration,
}

impl ServiceTelemetry {
    pub fn new(now: Instant, qos_fps: u32, spf: Duration) -> Self {
        Self {
            started: now,
            qos_latest: qos_fps,
            qos_min: qos_fps,
            qos_max: qos_fps,
            spf,
            capture_calls: 0,
            capture_ok: 0,
            capture_would_block: 0,
            capture_errors: 0,
            capture_time: DurationStat::default(),
            encode_calls: 0,
            encoded_frames: 0,
            encode_errors: 0,
            encode_time: DurationStat::default(),
            dispatch_attempt_messages: 0,
            dispatch_attempt_frames: 0,
            payload_bytes: 0,
            fetched_wait: DurationStat::default(),
            sleep: DurationStat::default(),
        }
    }

    pub fn qos(&mut self, fps: u32, spf: Duration) {
        self.qos_latest = fps;
        self.qos_min = self.qos_min.min(fps);
        self.qos_max = self.qos_max.max(fps);
        self.spf = spf;
    }

    pub fn capture(&mut self, elapsed: Duration, outcome: CaptureOutcome) {
        self.capture_calls += 1;
        self.capture_time.add(elapsed);
        match outcome {
            CaptureOutcome::Ok => self.capture_ok += 1,
            CaptureOutcome::WouldBlock => self.capture_would_block += 1,
            CaptureOutcome::Error => self.capture_errors += 1,
        }
    }

    pub fn encode(&mut self, elapsed: Duration, frames: usize, payload_bytes: usize, ok: bool) {
        self.encode_calls += 1;
        self.encode_time.add(elapsed);
        if ok {
            self.encoded_frames += frames as u64;
            self.payload_bytes = self.payload_bytes.saturating_add(payload_bytes as u64);
        } else {
            self.encode_errors += 1;
        }
    }

    pub fn dispatch_attempt(&mut self, messages: usize, frames_per_message: usize) {
        self.dispatch_attempt_messages += messages as u64;
        self.dispatch_attempt_frames = self
            .dispatch_attempt_frames
            .saturating_add((messages as u64).saturating_mul(frames_per_message as u64));
    }

    pub fn fetched_wait(&mut self, elapsed: Duration) {
        self.fetched_wait.add(elapsed);
    }
    pub fn sleep(&mut self, elapsed: Duration) {
        self.sleep.add(elapsed);
    }

    pub fn take_if_due(&mut self, now: Instant) -> Option<ServiceSnapshot> {
        let elapsed = now.duration_since(self.started);
        if elapsed < Duration::from_secs(1) {
            return None;
        }
        let next = Self::new(now, self.qos_latest, self.spf);
        let old = std::mem::replace(self, next);
        Some(ServiceSnapshot {
            elapsed,
            qos_latest: old.qos_latest,
            qos_min: old.qos_min,
            qos_max: old.qos_max,
            spf: old.spf,
            capture_calls: old.capture_calls,
            capture_ok: old.capture_ok,
            capture_would_block: old.capture_would_block,
            capture_errors: old.capture_errors,
            capture_total: old.capture_time.total,
            capture_max: old.capture_time.max,
            encode_calls: old.encode_calls,
            encoded_frames: old.encoded_frames,
            encode_errors: old.encode_errors,
            encode_total: old.encode_time.total,
            encode_max: old.encode_time.max,
            dispatch_attempt_messages: old.dispatch_attempt_messages,
            dispatch_attempt_frames: old.dispatch_attempt_frames,
            payload_bytes: old.payload_bytes,
            fetched_wait_total: old.fetched_wait.total,
            fetched_wait_max: old.fetched_wait.max,
            sleep_total: old.sleep.total,
            sleep_max: old.sleep.max,
        })
    }
}

pub struct TransportTelemetry {
    started: Instant,
    pub ordinal: u64,
    display: Option<usize>,
    mixed_displays: bool,
    messages_ok: u64,
    frames_ok: u64,
    payload_bytes: u64,
    failures: u64,
    send_time: DurationStat,
}

pub struct TransportSnapshot {
    pub elapsed: Duration,
    pub display: i64,
    pub messages_ok: u64,
    pub frames_ok: u64,
    pub payload_bytes: u64,
    pub failures: u64,
    pub send_total: Duration,
    pub send_max: Duration,
}

impl TransportTelemetry {
    pub fn new(now: Instant, ordinal: u64) -> Self {
        Self {
            started: now,
            ordinal,
            display: None,
            mixed_displays: false,
            messages_ok: 0,
            frames_ok: 0,
            payload_bytes: 0,
            failures: 0,
            send_time: DurationStat::default(),
        }
    }

    pub fn send_result(
        &mut self,
        elapsed: Duration,
        display: usize,
        frames: usize,
        payload_bytes: usize,
        ok: bool,
    ) {
        self.send_time.add(elapsed);
        if let Some(previous) = self.display {
            self.mixed_displays |= previous != display;
        } else {
            self.display = Some(display);
        }
        if ok {
            self.messages_ok += 1;
            self.frames_ok += frames as u64;
            self.payload_bytes = self.payload_bytes.saturating_add(payload_bytes as u64);
        } else {
            self.failures += 1;
        }
    }

    pub fn take_if_due(&mut self, now: Instant) -> Option<TransportSnapshot> {
        let elapsed = now.duration_since(self.started);
        if elapsed < Duration::from_secs(1) {
            return None;
        }
        let next = Self::new(now, self.ordinal);
        let old = std::mem::replace(self, next);
        Some(TransportSnapshot {
            elapsed,
            display: if old.mixed_displays {
                -1
            } else {
                old.display.map(|display| display as i64).unwrap_or(-1)
            },
            messages_ok: old.messages_ok,
            frames_ok: old.frames_ok,
            payload_bytes: old.payload_bytes,
            failures: old.failures,
            send_total: old.send_time.total,
            send_max: old.send_time.max,
        })
    }

    pub fn finish(&mut self, now: Instant) -> Option<TransportSnapshot> {
        if self.messages_ok == 0 && self.failures == 0 {
            return None;
        }
        let elapsed = now.duration_since(self.started);
        let next = Self::new(now, self.ordinal);
        let old = std::mem::replace(self, next);
        Some(TransportSnapshot {
            elapsed,
            display: if old.mixed_displays {
                -1
            } else {
                old.display.map(|display| display as i64).unwrap_or(-1)
            },
            messages_ok: old.messages_ok,
            frames_ok: old.frames_ok,
            payload_bytes: old.payload_bytes,
            failures: old.failures,
            send_total: old.send_time.total,
            send_max: old.send_time.max,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_window_uses_real_elapsed_and_resets_multiframe_counts() {
        let start = Instant::now();
        let mut stats = ServiceTelemetry::new(start, 120, Duration::from_micros(8_333));
        stats.capture(Duration::from_millis(3), CaptureOutcome::Ok);
        stats.encode(Duration::from_millis(4), 3, 900, true);
        stats.dispatch_attempt(2, 3);
        assert!(stats
            .take_if_due(start + Duration::from_millis(999))
            .is_none());
        let snapshot = stats
            .take_if_due(start + Duration::from_millis(1500))
            .unwrap();
        assert_eq!(snapshot.elapsed, Duration::from_millis(1500));
        assert_eq!(
            (snapshot.encoded_frames, snapshot.dispatch_attempt_frames),
            (3, 6)
        );
        assert_eq!(snapshot.payload_bytes, 900);
        let reset = stats
            .take_if_due(start + Duration::from_millis(2500))
            .unwrap();
        assert_eq!(
            (
                reset.capture_calls,
                reset.encode_calls,
                reset.dispatch_attempt_messages
            ),
            (0, 0, 0)
        );
    }

    #[test]
    fn transport_partial_close_tracks_failure_and_mixed_display() {
        let start = Instant::now();
        let mut stats = TransportTelemetry::new(start, 7);
        stats.send_result(Duration::from_millis(2), 3, 4, 1000, false);
        stats.send_result(Duration::from_millis(1), 4, 1, 10, true);
        let snapshot = stats.finish(start + Duration::from_millis(100)).unwrap();
        assert_eq!(snapshot.elapsed, Duration::from_millis(100));
        assert_eq!(snapshot.display, -1);
        assert_eq!(
            (
                snapshot.messages_ok,
                snapshot.frames_ok,
                snapshot.payload_bytes
            ),
            (1, 1, 10)
        );
        assert_eq!(snapshot.failures, 1);
    }
}
