//! Automatic updates through Sparkle.
//!
//! Release bundles embed `Sparkle.framework` (see `scripts/bundle.sh`); its feed URL
//! and EdDSA public key are in Info.plist. The framework is loaded at runtime, so
//! development builds without it run normally, just without updates.
//!
//! Sparkle owns the whole update flow: scheduled checks, its own "update available"
//! window, signature and code-signature verification, replacing the complete `.app`
//! and relaunching.
//!
//! Other platforms have no updater yet: [`is_available`] is false there.

#[cfg(target_os = "macos")]
pub use sparkle::*;

#[cfg(not(target_os = "macos"))]
pub use unsupported::*;

#[cfg(target_os = "macos")]
mod sparkle {
    use std::cell::Cell;
    use std::ptr::null_mut;

    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2_foundation::{NSBundle, NSString};

    thread_local! {
        /// The `SPUStandardUpdaterController`, created once on the main thread and kept
        /// for the life of the app.
        static CONTROLLER: Cell<*mut AnyObject> = const { Cell::new(null_mut()) };
    }

    /// Load Sparkle and start its updater (scheduled checks begin). Call once at launch,
    /// on the main thread. Returns whether updates are available in this build.
    pub fn start() -> bool {
        if is_available() {
            return true;
        }
        let Some(controller) = create_controller() else {
            return false;
        };
        CONTROLLER.with(|c| c.set(controller));
        true
    }

    /// Whether this build can update itself (a release bundle with Sparkle).
    pub fn is_available() -> bool {
        CONTROLLER.with(|c| !c.get().is_null())
    }

    /// Show Sparkle's "Check for Updates" flow (it reports "up to date" too).
    pub fn check_for_updates() {
        let controller = CONTROLLER.with(|c| c.get());
        if controller.is_null() {
            return;
        }
        // SAFETY: `controller` is a live SPUStandardUpdaterController we retain forever;
        // `checkForUpdates:` takes an optional sender.
        unsafe {
            let _: () = msg_send![controller, checkForUpdates: null_mut::<AnyObject>()];
        }
    }

    /// Whether Sparkle checks for updates on its own schedule (the user's choice,
    /// stored by Sparkle). `None` when updates aren't available in this build.
    pub fn automatically_checks() -> Option<bool> {
        let updater = updater()?;
        // SAFETY: `updater` is the controller's SPUUpdater; the property is a BOOL.
        let value: Bool = unsafe { msg_send![updater, automaticallyChecksForUpdates] };
        Some(value.as_bool())
    }

    pub fn set_automatically_checks(enabled: bool) {
        if let Some(updater) = updater() {
            // SAFETY: as above; the setter takes a BOOL.
            unsafe {
                let _: () = msg_send![updater, setAutomaticallyChecksForUpdates: Bool::new(enabled)];
            }
        }
    }

    fn updater() -> Option<*mut AnyObject> {
        let controller = CONTROLLER.with(|c| c.get());
        if controller.is_null() {
            return None;
        }
        // SAFETY: `updater` is a readonly property returning a non-null SPUUpdater.
        let updater: *mut AnyObject = unsafe { msg_send![controller, updater] };
        (!updater.is_null()).then_some(updater)
    }

    fn create_controller() -> Option<*mut AnyObject> {
        let frameworks = NSBundle::mainBundle().privateFrameworksPath()?;
        let path = format!("{frameworks}/Sparkle.framework");
        if !std::path::Path::new(&path).exists() {
            log::info!("Sparkle.framework not bundled; automatic updates disabled");
            return None;
        }
        let bundle = NSBundle::bundleWithPath(&NSString::from_str(&path))?;
        // SAFETY: loading a signed framework from our own bundle.
        if !unsafe { bundle.load() } {
            log::warn!("couldn't load {path}; automatic updates disabled");
            return None;
        }
        let class = AnyClass::get(c"SPUStandardUpdaterController")?;
        // SAFETY: `-initWithStartingUpdater:updaterDelegate:userDriverDelegate:` is the
        // designated initializer; nil delegates use Sparkle's standard behavior. The
        // returned object is +1 retained and deliberately never released.
        unsafe {
            let allocated: *mut AnyObject = msg_send![class, alloc];
            let controller: *mut AnyObject = msg_send![
                allocated,
                initWithStartingUpdater: Bool::YES,
                updaterDelegate: null_mut::<AnyObject>(),
                userDriverDelegate: null_mut::<AnyObject>()
            ];
            (!controller.is_null()).then_some(controller)
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod unsupported {
    pub fn start() -> bool {
        false
    }

    pub fn is_available() -> bool {
        false
    }

    pub fn check_for_updates() {}

    pub fn automatically_checks() -> Option<bool> {
        None
    }

    pub fn set_automatically_checks(_enabled: bool) {}
}
