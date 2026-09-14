//! A fake `adb` serving one display's media directory from the sandbox. It
//! understands exactly the commands tryxctl sends and fails loudly on
//! anything else, so a new command cannot slip past the tests untested.

use crate::sandbox::Sandbox;
use std::path::PathBuf;

const SCRIPT: &str = r#"#!/bin/sh
# A fake adb for tryxctl's tests, serving one display from a directory.
root='@ROOT@'
media="$root/pcMedia"
printf '%s\n' "$*" >> "$root/calls"
serial=$(cat "$root/serial")
if [ "$1" = "-s" ]; then
    if [ "$2" != "$serial" ]; then
        echo "adb: device '$2' not found" >&2
        exit 1
    fi
    shift 2
fi
verb=$1
if [ -f "$root/fail-$verb" ]; then
    cat "$root/fail-$verb" >&2
    exit 1
fi
# The sandbox path of a file in the display's media directory.
local_path() {
    case "$1" in
        /sdcard/pcMedia/*) printf '%s/%s' "$media" "${1#/sdcard/pcMedia/}" ;;
        *) echo "fake adb: $1 is outside the media directory" >&2; exit 1 ;;
    esac
}
case "$verb" in
    devices)
        echo "List of devices attached"
        if [ -f "$root/listing" ]; then
            cat "$root/listing"
        else
            printf '%s\t%s usb:%s product:cm01_se model:cm01_se device:cm01 transport_id:1\n' \
                "$serial" "$(cat "$root/state")" "$(cat "$root/usb")"
        fi
        echo
        ;;
    shell)
        set -f
        case "$2" in
            "stat -c '%s %n' /sdcard/pcMedia/* 2>/dev/null; true")
                set +f
                for file in "$media"/*; do
                    [ -f "$file" ] || continue
                    printf '%s /sdcard/pcMedia/%s\n' "$(wc -c < "$file" | tr -d ' ')" "${file##*/}"
                done
                ;;
            "df -k /sdcard")
                echo "Filesystem     1K-blocks    Used Available Use% Mounted on"
                printf '/dev/fuse       11681792 4218880 %s  37%% /storage/emulated\n' "$(cat "$root/available")"
                ;;
            "mv -f -- "*)
                set -- $2
                from=$(local_path "$4") && to=$(local_path "$5") || exit 1
                if [ ! -f "$from" ]; then
                    echo "mv: bad '$4': No such file or directory"
                    exit 1
                fi
                mv -f "$from" "$to"
                ;;
            "rm -- "*)
                set -- $2
                path=$(local_path "$3") || exit 1
                if [ ! -f "$path" ]; then
                    echo "rm: $3: No such file or directory"
                    exit 1
                fi
                rm -f "$path"
                ;;
            *)
                echo "fake adb: unsupported shell command: $2" >&2
                exit 1
                ;;
        esac
        ;;
    push)
        to=$(local_path "$3") || exit 1
        if [ -f "$root/short-push" ]; then
            head -c "$(cat "$root/short-push")" "$2" > "$to"
        else
            cp "$2" "$to"
        fi
        echo "$2: 1 file pushed, 0 skipped."
        ;;
    pull)
        from=$(local_path "$2") || exit 1
        if [ ! -f "$from" ]; then
            echo "adb: error: failed to stat remote object '$2': No such file or directory" >&2
            exit 1
        fi
        to=$3
        if [ -d "$to" ]; then
            to="$to/${from##*/}"
        fi
        if [ -f "$root/short-pull" ]; then
            head -c "$(cat "$root/short-pull")" "$from" > "$to"
        else
            cp "$from" "$to"
        fi
        echo "$2: 1 file pulled, 0 skipped."
        ;;
    exec-out)
        # exec-out head -c COUNT -- PATH
        path=$(local_path "$6") || exit 1
        head -c "$4" "$path"
        ;;
    *)
        echo "fake adb: unsupported command: $*" >&2
        exit 1
        ;;
esac
"#;

pub struct FakeAdb {
    root: PathBuf,
}

impl FakeAdb {
    /// Installs `adb` into the sandbox's bin directory, answering for a
    /// display with USB serial `serial` on USB port `usb` (its sysfs name,
    /// such as `3-12`).
    pub fn install(sandbox: &Sandbox, serial: &str, usb: &str) -> FakeAdb {
        let root = sandbox.root().join("adb");
        std::fs::create_dir_all(root.join("pcMedia")).expect("the fake media directory");
        let adb = FakeAdb { root };
        adb.store("serial", serial);
        adb.store("usb", usb);
        adb.set_state("device");
        adb.set_available_kib(7_462_912);
        let script = sandbox.bin().join("adb");
        let staged = script.with_extension("new");
        std::fs::write(
            &staged,
            SCRIPT.replace("@ROOT@", &adb.root.to_string_lossy()),
        )
        .expect("the fake adb script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))
                .expect("an executable script");
        }
        std::fs::rename(&staged, &script).expect("the fake adb in place");
        adb
    }

    fn store(&self, name: &str, value: &str) {
        std::fs::write(self.root.join(name), value).expect("fake adb state");
    }

    fn forget(&self, name: &str) {
        let _ = std::fs::remove_file(self.root.join(name));
    }

    /// The display's media directory.
    pub fn media(&self) -> PathBuf {
        self.root.join("pcMedia")
    }

    /// Puts a file on the display.
    pub fn put(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.media().join(name), bytes).expect("a file on the display");
    }

    /// A file on the display.
    pub fn read(&self, name: &str) -> Option<Vec<u8>> {
        std::fs::read(self.media().join(name)).ok()
    }

    /// The files on the display, sorted by name.
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.media())
            .expect("the fake media directory")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// The transport state `adb devices` reports: `device`, `unauthorized`,
    /// `offline`, or `no permissions (…)`.
    pub fn set_state(&self, state: &str) {
        self.store("state", state);
    }

    /// Replaces the device lines of `adb devices -l`, or restores the
    /// display's own line with `None`.
    pub fn set_listing(&self, lines: Option<&str>) {
        match lines {
            Some(lines) => self.store("listing", lines),
            None => self.forget("listing"),
        }
    }

    /// Free space on the display's storage.
    pub fn set_available_kib(&self, kib: u64) {
        self.store("available", &kib.to_string());
    }

    /// Makes every `adb <verb>` fail with `message`, or work again with `None`.
    pub fn fail(&self, verb: &str, message: Option<&str>) {
        match message {
            Some(message) => self.store(&format!("fail-{verb}"), message),
            None => self.forget(&format!("fail-{verb}")),
        }
    }

    /// Makes pushes stop after `bytes`, or copy whole files again with `None`.
    pub fn truncate_pushes(&self, bytes: Option<u64>) {
        match bytes {
            Some(bytes) => self.store("short-push", &bytes.to_string()),
            None => self.forget("short-push"),
        }
    }

    /// Makes pulls stop after `bytes`, or copy whole files again with `None`.
    pub fn truncate_pulls(&self, bytes: Option<u64>) {
        match bytes {
            Some(bytes) => self.store("short-pull", &bytes.to_string()),
            None => self.forget("short-pull"),
        }
    }

    /// The arguments of every call so far, one line each.
    pub fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.root.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}
