use std::ffi::OsString;
use std::path::Path;

use byte::desktop::paths::{DesktopPaths, RealDesktopPaths, TestDesktopPaths};
use byte::paths::{HostPaths, TestPaths};

#[test]
fn the_default_location_is_under_appdata() {
    // Mirrors RealPaths::resolve: pure path arithmetic, so the mapping is
    // testable without touching process-global environment variables.
    // Uses forward slashes to work cross-platform; Path::PartialEq compares
    // components, and backslashes are literal characters on non-Windows.
    let appdata = Path::new("C:/Users/x/AppData/Roaming");
    let p = RealDesktopPaths::resolve(appdata, None);
    assert_eq!(p.desktop_dir(), appdata.join("Claude"));
}

#[test]
fn the_override_replaces_the_whole_directory() {
    // When an override is provided, it replaces the whole directory path,
    // not appended to appdata. Uses forward slashes for cross-platform
    // component compatibility.
    let over = OsString::from("D:/elsewhere/Claude");
    let p = RealDesktopPaths::resolve(Path::new("C:/Users/x/AppData/Roaming"), Some(&over));
    assert_eq!(p.desktop_dir(), Path::new("D:/elsewhere/Claude"));
}

#[test]
fn the_config_file_sits_directly_in_the_desktop_directory() {
    // config.json is patched in place, never moved, so its location must be
    // derived from desktop_dir rather than stored separately.
    let p = RealDesktopPaths::resolve(Path::new("/home/x"), None);
    assert_eq!(p.config_file(), p.desktop_dir().join("config.json"));
}

#[test]
fn test_paths_are_rooted_in_a_temp_directory_that_exists() {
    let p = TestDesktopPaths::new().unwrap();
    assert!(p.desktop_dir().is_dir(), "{:?}", p.desktop_dir());
    assert!(p.desktop_dir().starts_with(p.root()));
}

#[test]
fn the_profile_store_and_journal_live_under_bytes_config_dir() {
    // Parked profiles are byte's own state, not the app's, so they belong
    // beside accounts.json rather than inside %APPDATA%\Claude.
    let tp = TestPaths::new().unwrap();
    assert_eq!(tp.desktop_store_dir(), tp.byte_config_dir().join("desktop"));
    assert_eq!(
        tp.desktop_journal_file(),
        tp.desktop_store_dir().join("journal.json")
    );
}

#[test]
fn each_account_gets_its_own_profile_directory_keyed_by_uuid() {
    let tp = TestPaths::new().unwrap();
    let a = tp.desktop_profile_dir("uuid-1");
    let b = tp.desktop_profile_dir("uuid-2");
    assert_ne!(a, b, "two accounts must not share a profile directory");
    assert!(a.starts_with(tp.desktop_store_dir()));
    assert!(a.ends_with("uuid-1"));
}
