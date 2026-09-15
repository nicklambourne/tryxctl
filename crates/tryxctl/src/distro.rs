//! The Linux distribution tryxctl runs on, read from os-release(5), so that
//! `doctor` can give that distribution's commands.

use std::collections::HashMap;
use std::path::PathBuf;

/// Names a file to read in place of `/etc/os-release`. Tests point it at one
/// they write, so no hint depends on the host the tests run on.
pub const OS_RELEASE_OVERRIDE: &str = "TRYXCTL_OS_RELEASE";

/// The distributions `doctor` has instructions for. A derivative counts as
/// the first family it names in `ID_LIKE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Debian, Ubuntu, and derivatives such as Linux Mint and Pop!_OS.
    Debian,
    /// Fedora and its spins. The RHEL rebuilds list fedora too, but RPM
    /// Fusion installs differently there, so they are left out.
    Fedora,
    /// Arch Linux and derivatives such as Manjaro and EndeavourOS.
    Arch,
    OpenSuseTumbleweed,
    OpenSuseLeap,
    NixOs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Distro {
    /// `PRETTY_NAME`, falling back to `NAME` and then `ID`.
    pub name: String,
    pub family: Option<Family>,
}

/// The distribution os-release describes, or `None` when there is no
/// os-release to read, as on macOS.
pub fn detect() -> Option<Distro> {
    let paths = match std::env::var_os(OS_RELEASE_OVERRIDE) {
        Some(path) => vec![PathBuf::from(path)],
        None => vec![
            PathBuf::from("/etc/os-release"),
            PathBuf::from("/usr/lib/os-release"),
        ],
    };
    paths
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .map(|text| parse(&text))
}

/// Reads os-release's `KEY=value` lines.
pub fn parse(text: &str) -> Distro {
    let fields: HashMap<&str, String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim(), unquote(value.trim())))
        .collect();
    let field = |key: &str| fields.get(key).map(String::as_str).unwrap_or_default();
    let name = [field("PRETTY_NAME"), field("NAME"), field("ID")]
        .into_iter()
        .find(|name| !name.is_empty())
        .unwrap_or("Linux");
    Distro {
        name: name.to_string(),
        family: family(field("ID"), field("ID_LIKE")),
    }
}

/// Strips the quotes os-release allows around a value, and the backslash
/// escapes allowed inside double quotes.
fn unquote(value: &str) -> String {
    if let Some(inner) = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        let mut unescaped = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            unescaped.push(if c == '\\' {
                chars.next().unwrap_or(c)
            } else {
                c
            });
        }
        return unescaped;
    }
    value
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
        .unwrap_or(value)
        .to_string()
}

/// The family `ID` names, or failing that the closest one in `ID_LIKE`.
fn family(id: &str, id_like: &str) -> Option<Family> {
    for name in std::iter::once(id).chain(id_like.split_whitespace()) {
        let family = match name {
            "debian" | "ubuntu" | "raspbian" => Family::Debian,
            "fedora" => Family::Fedora,
            "arch" => Family::Arch,
            "opensuse-leap" => Family::OpenSuseLeap,
            "opensuse-tumbleweed" | "opensuse-slowroll" | "opensuse" => Family::OpenSuseTumbleweed,
            "nixos" => Family::NixOs,
            // RHEL and its rebuilds, and SUSE Linux Enterprise, get these
            // packages some other way than the family they are like.
            "rhel" | "centos" | "sles" | "sled" => return None,
            _ => continue,
        };
        return Some(family);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_quoted_values_and_skips_comments() {
        let distro = parse(
            "# a comment\nNAME=\"Fedora Linux\"\nID=fedora\n\nPRETTY_NAME=\"Fedora Linux 42 (Workstation \\\"Edition\\\")\"\nVERSION_ID=42\n",
        );
        assert_eq!(distro.name, "Fedora Linux 42 (Workstation \"Edition\")");
        assert_eq!(distro.family, Some(Family::Fedora));
        assert_eq!(parse("NAME='Gentoo'\nID=gentoo").name, "Gentoo");
        assert_eq!(parse("ID=gentoo").name, "gentoo");
        assert_eq!(parse("").name, "Linux");
        assert_eq!(unquote("\""), "\"");
    }

    #[test]
    fn derivatives_resolve_to_the_family_they_are_like() {
        for (id, id_like, family) in [
            ("debian", "", Some(Family::Debian)),
            ("ubuntu", "debian", Some(Family::Debian)),
            ("linuxmint", "ubuntu debian", Some(Family::Debian)),
            ("pop", "ubuntu debian", Some(Family::Debian)),
            ("raspbian", "debian", Some(Family::Debian)),
            ("fedora", "", Some(Family::Fedora)),
            ("fedora-asahi-remix", "fedora", Some(Family::Fedora)),
            ("rocky", "rhel centos fedora", None),
            ("arch", "", Some(Family::Arch)),
            ("manjaro", "arch", Some(Family::Arch)),
            ("endeavouros", "arch", Some(Family::Arch)),
            (
                "opensuse-tumbleweed",
                "opensuse suse",
                Some(Family::OpenSuseTumbleweed),
            ),
            ("opensuse-leap", "suse opensuse", Some(Family::OpenSuseLeap)),
            ("sles", "suse", None),
            ("nixos", "", Some(Family::NixOs)),
            ("gentoo", "", None),
            ("", "", None),
        ] {
            assert_eq!(super::family(id, id_like), family, "{id} like {id_like:?}");
        }
    }
}
