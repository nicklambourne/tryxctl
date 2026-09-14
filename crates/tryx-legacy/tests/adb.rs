//! Media transfer through a fake adb. Each test runs in its own process with
//! the sandbox's bin directory first on `PATH`, where `Adb::new` finds the
//! fake.

use tryx_legacy::LegacyError;
use tryx_legacy::adb::{self, Adb, DiskUsage, MediaFile};
use tryx_testkit::{FakeAdb, isolated};

const SERIAL: &str = "XYZ000000000000001";

#[test]
fn lists_pushes_pulls_renames_and_removes_media() {
    isolated("lists_pushes_pulls_renames_and_removes_media", |sandbox| {
        let fake = FakeAdb::install(sandbox, SERIAL, "3-12");
        fake.put("clip.mp4", &[7u8; 2048]);
        let adb = Adb::new().unwrap();
        let devices = adb.devices().unwrap();
        let display = adb::select(&devices, Some(SERIAL), Some("3-12")).unwrap();
        assert_eq!(display.state, "device");
        let adb = adb.with_serial(display.serial.clone());

        assert_eq!(
            adb.list_media().unwrap(),
            [MediaFile {
                name: "clip.mp4".into(),
                size: 2048
            }]
        );
        assert_eq!(
            adb.free_space().unwrap(),
            DiskUsage {
                total_kib: 11_681_792,
                used_kib: 4_218_880,
                available_kib: 7_462_912
            }
        );

        let local = sandbox.work().join("still.png");
        std::fs::write(&local, b"not really a png").unwrap();
        adb.push(&local, "still.png").unwrap();
        assert_eq!(fake.read("still.png").unwrap(), b"not really a png");

        let back = sandbox.work().join("copy.mp4");
        adb.pull("clip.mp4", &back).unwrap();
        assert_eq!(std::fs::read(&back).unwrap(), [7u8; 2048]);
        assert_eq!(adb.read_prefix("clip.mp4", 100).unwrap(), [7u8; 100]);

        adb.rename("still.png", "renamed.png").unwrap();
        assert_eq!(fake.names(), ["clip.mp4", "renamed.png"]);
        assert!(matches!(
            adb.rename("gone.png", "other.png"),
            Err(LegacyError::Adb { .. })
        ));

        assert!(adb.remove("renamed.png").unwrap());
        assert!(!adb.remove("renamed.png").unwrap(), "already gone");
        assert_eq!(fake.names(), ["clip.mp4"]);

        // Every device command named the display's transport.
        for call in fake
            .calls()
            .iter()
            .filter(|call| !call.starts_with("devices"))
        {
            assert!(call.starts_with(&format!("-s {SERIAL} ")), "{call}");
        }
    });
}

#[test]
fn unsafe_names_never_reach_adb() {
    isolated("unsafe_names_never_reach_adb", |sandbox| {
        let fake = FakeAdb::install(sandbox, SERIAL, "3-12");
        let adb = Adb::new().unwrap().with_serial(SERIAL);
        let local = sandbox.work().join("x.png");
        std::fs::write(&local, b"x").unwrap();
        for name in ["../x.png", "a b.png", "$(reboot).png", ".hidden", ""] {
            assert!(matches!(
                adb.push(&local, name),
                Err(LegacyError::UnsafeMediaName(_))
            ));
            assert!(matches!(
                adb.remove(name),
                Err(LegacyError::UnsafeMediaName(_))
            ));
        }
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    });
}

#[test]
fn adb_failures_carry_their_message() {
    isolated("adb_failures_carry_their_message", |sandbox| {
        let fake = FakeAdb::install(sandbox, SERIAL, "3-12");
        let adb = Adb::new().unwrap().with_serial(SERIAL);
        fake.fail(
            "push",
            Some("adb: error: failed to copy: No space left on device"),
        );
        let local = sandbox.work().join("x.png");
        std::fs::write(&local, b"x").unwrap();
        match adb.push(&local, "x.png") {
            Err(LegacyError::Adb { args, message }) => {
                assert!(args.starts_with("push "), "{args}");
                assert!(message.contains("No space left"), "{message}");
            }
            other => panic!("{other:?}"),
        }
        // A transport that went away.
        let gone = Adb::new().unwrap().with_serial("OTHER");
        assert!(
            matches!(gone.list_media(), Err(LegacyError::Adb { message, .. }) if message.contains("not found"))
        );
        fake.fail("shell", Some("error: closed"));
        assert!(adb.free_space().is_err());
        assert!(matches!(
            adb.read_prefix("x.png", 10),
            Err(LegacyError::Adb { .. })
        ));
    });
}

#[test]
fn without_adb_on_the_path_it_is_missing() {
    isolated("without_adb_on_the_path_it_is_missing", |sandbox| {
        // SAFETY: an isolated test runs alone in its own process.
        unsafe { std::env::set_var("PATH", sandbox.bin()) };
        assert!(matches!(Adb::new(), Err(LegacyError::AdbMissing)));
    });
}
