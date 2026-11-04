//! Motion: one easing primitive, and a frame clock that only runs while
//! something is moving, so a settled window costs nothing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::Thread;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// Motion the user is driving (scroll, hover, view changes) runs at the
/// panel's own rate, up to about 160 fps. These gaps must stay a little under
/// a whole number of refreshes, or a 144 Hz panel falls to an uneven 48 fps.
const FINE: Duration = Duration::from_millis(6);
/// Motion nobody is steering: data settling, a long job's spinner.
const AMBIENT: Duration = Duration::from_millis(30);

/// A value gliding toward its target. Exponential decay can be retargeted
/// mid-flight without a visible jump, which is what hover and scroll need.
#[derive(Clone, Copy, Debug, Default)]
pub struct Ease {
    pub value: f32,
    pub target: f32,
}

impl Ease {
    pub const fn new(v: f32) -> Self {
        Ease { value: v, target: v }
    }

    pub fn snap(&mut self, v: f32) {
        *self = Ease::new(v);
    }

    /// Advances by `dt` seconds with time constant `tau`, landing exactly on
    /// the target once within `eps`. Returns whether it is still moving.
    pub fn step(&mut self, dt: f32, tau: f32, eps: f32) -> bool {
        self.value += (self.target - self.value) * (1.0 - (-dt / tau).exp());
        if (self.target - self.value).abs() <= eps {
            self.value = self.target;
            return false;
        }
        true
    }
}

/// A critically damped spring. Unlike `Ease` it starts from rest and carries
/// its speed through retargeting, so a wheel spun notch after notch reads as
/// one continuous glide rather than a series of kicks.
#[derive(Clone, Copy, Debug, Default)]
pub struct Spring {
    pub value: f32,
    pub target: f32,
    velocity: f32,
}

impl Spring {
    /// Advances by `dt` seconds. `stiffness` is in 1/s: higher settles sooner.
    /// This is the exact solution, so no frame time can make it overshoot.
    pub fn step(&mut self, dt: f32, stiffness: f32, eps: f32) -> bool {
        let offset = self.value - self.target;
        let decay = (-stiffness * dt).exp();
        let drive = (self.velocity + stiffness * offset) * dt;
        self.value = self.target + (offset + drive) * decay;
        self.velocity = (self.velocity - drive * stiffness) * decay;
        if (self.target - self.value).abs() <= eps && self.velocity.abs() <= eps * stiffness {
            self.value = self.target;
            self.velocity = 0.0;
            return false;
        }
        true
    }
}

struct Clock {
    running: AtomicBool,
    queued: AtomicBool,
    fine: AtomicBool,
}

/// Posts a message once per displayed frame while running. Paced by the
/// compositor, so frames line up with the screen instead of a coarse timer.
pub struct Ticker {
    clock: Arc<Clock>,
    thread: Thread,
}

impl Ticker {
    pub fn start(hwnd: HWND, msg: u32) -> Self {
        let clock = Arc::new(Clock {
            running: AtomicBool::new(false),
            queued: AtomicBool::new(false),
            fine: AtomicBool::new(true),
        });
        let shared = clock.clone();
        let hwnd = hwnd.0 as usize;
        let thread = std::thread::spawn(move || {
            let mut last = Instant::now();
            loop {
                while !shared.running.load(Ordering::Acquire) {
                    std::thread::park();
                }
                if unsafe { DwmFlush() }.is_err() {
                    std::thread::sleep(AMBIENT);
                }
                let gap = if shared.fine.load(Ordering::Acquire) { FINE } else { AMBIENT };
                // One frame in flight at most: a slow paint skips frames, it never queues them.
                if last.elapsed() >= gap && !shared.queued.swap(true, Ordering::AcqRel) {
                    last = Instant::now();
                    unsafe { PostMessageW(Some(HWND(hwnd as _)), msg, WPARAM(0), LPARAM(0)) }.ok();
                }
            }
        })
        .thread()
        .clone();
        Ticker { clock, thread }
    }

    pub fn running(&self) -> bool {
        self.clock.running.load(Ordering::Acquire)
    }

    pub fn run(&self) {
        if !self.clock.running.swap(true, Ordering::AcqRel) {
            self.thread.unpark();
        }
    }

    /// Whether the next frames should run at the panel's full rate.
    pub fn pace(&self, fine: bool) {
        self.clock.fine.store(fine, Ordering::Release);
    }

    pub fn stop(&self) {
        self.clock.running.store(false, Ordering::Release);
    }

    /// The posted frame has been handled.
    pub fn ack(&self) {
        self.clock.queued.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::{Ease, Spring};

    #[test]
    fn settles_exactly_on_target() {
        let mut e = Ease::new(0.0);
        e.target = 100.0;
        let mut frames = 0;
        while e.step(1.0 / 60.0, 0.1, 0.05) {
            frames += 1;
            assert!(frames < 120, "never settled");
        }
        assert_eq!(e.value, 100.0);
    }

    #[test]
    fn a_huge_step_lands_at_once() {
        // How animations are switched off for people who disable them in Windows.
        let mut e = Ease::new(0.0);
        e.target = 1.0;
        assert!(!e.step(10.0, 0.1, 0.001));
        assert_eq!(e.value, 1.0);
    }

    #[test]
    fn a_spring_starts_gently_and_never_overshoots() {
        let mut s = Spring { target: 72.0, ..Default::default() };
        s.step(1.0 / 144.0, 24.0, 0.2);
        assert!(s.value < 1.0, "first frame moved {}", s.value);
        let mut last = s.value;
        while s.step(1.0 / 144.0, 24.0, 0.2) {
            assert!(s.value >= last && s.value <= 72.0);
            last = s.value;
        }
        assert_eq!(s.value, 72.0);
    }

    #[test]
    fn a_spring_lands_at_once_on_a_huge_step() {
        let mut s = Spring { target: 300.0, ..Default::default() };
        assert!(!s.step(10.0, 24.0, 0.2));
        assert_eq!(s.value, 300.0);
    }

    #[test]
    fn retargeting_does_not_jump() {
        let mut e = Ease::new(0.0);
        e.target = 1.0;
        e.step(1.0 / 60.0, 0.1, 0.001);
        let before = e.value;
        e.target = 0.0;
        e.step(1.0 / 60.0, 0.1, 0.001);
        assert!((e.value - before).abs() < 0.2);
    }
}
