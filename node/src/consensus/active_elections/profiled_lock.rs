use std::{
    collections::HashMap,
    convert::Infallible,
    ops::{Deref, DerefMut},
    panic::Location,
    sync::{Mutex, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard},
    time::Instant,
};

/// RAI evaluation: a read-write lock that, with `RAI_LOCK_PROFILE=1`, records
/// for every place that takes it how long it waited and how long it held the
/// lock. A queue behind this lock looks like an idle node; only the holders
/// tell which section the others wait for. Without the switch it is the plain
/// lock and reads no clock.
pub(crate) struct ProfiledRwLock<T> {
    inner: RwLock<T>,
}

#[derive(Clone, Copy, Default)]
struct SiteStats {
    count: u64,
    wait_us: u64,
    hold_us: u64,
    max_hold_us: u64,
    max_wait_us: u64,
}

type Site = (&'static str, u32, bool);

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("RAI_LOCK_PROFILE").is_ok_and(|value| value == "1"))
}

fn sites() -> &'static Mutex<HashMap<Site, SiteStats>> {
    static SITES: OnceLock<Mutex<HashMap<Site, SiteStats>>> = OnceLock::new();
    SITES.get_or_init(Default::default)
}

/// What a guard remembers to account for itself when dropped
struct Held {
    site: Site,
    wait_us: u64,
    since: Instant,
}

impl Held {
    fn start(
        location: &'static Location<'static>,
        write: bool,
        asked: Option<Instant>,
    ) -> Option<Self> {
        let asked = asked?;
        let since = Instant::now();
        Some(Self {
            site: (location.file(), location.line(), write),
            wait_us: since.duration_since(asked).as_micros() as u64,
            since,
        })
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        let hold_us = self.since.elapsed().as_micros() as u64;
        let mut sites = sites().lock().unwrap();
        let stats = sites.entry(self.site).or_default();
        stats.count += 1;
        stats.wait_us += self.wait_us;
        stats.hold_us += hold_us;
        stats.max_hold_us = stats.max_hold_us.max(hold_us);
        stats.max_wait_us = stats.max_wait_us.max(self.wait_us);
    }
}

pub(crate) struct ProfiledReadGuard<'a, T> {
    guard: RwLockReadGuard<'a, T>,
    _held: Option<Held>,
}

pub(crate) struct ProfiledWriteGuard<'a, T> {
    guard: RwLockWriteGuard<'a, T>,
    _held: Option<Held>,
}

impl<T> Deref for ProfiledReadGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T> Deref for ProfiledWriteGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T> DerefMut for ProfiledWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl<T> ProfiledRwLock<T> {
    pub(crate) fn new(value: T) -> Self {
        Self {
            inner: RwLock::new(value),
        }
    }

    /// As `RwLock::read`; the result is there for the callers' `unwrap`
    #[track_caller]
    pub(crate) fn read(&self) -> Result<ProfiledReadGuard<'_, T>, Infallible> {
        let location = Location::caller();
        let asked = enabled().then(Instant::now);
        let guard = self.inner.read().unwrap();
        Ok(ProfiledReadGuard {
            guard,
            _held: Held::start(location, false, asked),
        })
    }

    #[track_caller]
    pub(crate) fn write(&self) -> Result<ProfiledWriteGuard<'_, T>, Infallible> {
        let location = Location::caller();
        let asked = enabled().then(Instant::now);
        let guard = self.inner.write().unwrap();
        Ok(ProfiledWriteGuard {
            guard,
            _held: Held::start(location, true, asked),
        })
    }
}

/// RAI evaluation: a named section of work timed while the profile is on;
/// the time is recorded when the guard is dropped
pub(crate) struct Section {
    label: &'static str,
    since: Instant,
}

/// Starts timing a section; None without the switch
pub(crate) fn section(label: &'static str) -> Option<Section> {
    enabled().then(|| Section {
        label,
        since: Instant::now(),
    })
}

impl Drop for Section {
    fn drop(&mut self) {
        let us = self.since.elapsed().as_micros() as u64;
        let mut sites = sites().lock().unwrap();
        // A section is a site with no line, its time counted as held
        let stats = sites.entry((self.label, 0, false)).or_default();
        stats.count += 1;
        stats.hold_us += us;
        stats.max_hold_us = stats.max_hold_us.max(us);
    }
}

/// The places that held the lock longest since the last call, as
/// `line:r|w:count:hold_ms:max_hold_ms:wait_ms:max_wait_ms`, and the table
/// starts again. Empty without the switch.
pub(crate) fn take_lock_profile(top: usize) -> Vec<String> {
    if !enabled() {
        return Vec::new();
    }
    let taken = std::mem::take(&mut *sites().lock().unwrap());
    let mut rows: Vec<(Site, SiteStats)> = taken.into_iter().collect();
    rows.sort_by_key(|(_, stats)| std::cmp::Reverse(stats.hold_us + stats.wait_us));
    rows.into_iter()
        .take(top + 8)
        .map(|((file, line, write), stats)| {
            // A timed section is named by its label instead of a line
            let place = if line == 0 {
                file.to_owned()
            } else {
                line.to_string()
            };
            format!(
                "{}:{}:{}:{}:{}:{}:{}",
                place,
                if write { 'w' } else { 'r' },
                stats.count,
                stats.hold_us / 1000,
                stats.max_hold_us / 1000,
                stats.wait_us / 1000,
                stats.max_wait_us / 1000
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_guards_its_value() {
        let lock = ProfiledRwLock::new(1);
        *lock.write().unwrap() += 1;
        assert_eq!(*lock.read().unwrap(), 2);
    }
}
