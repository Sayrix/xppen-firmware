//! Release versioning, package verification and artifact assembly.
mod package;

use regex::Regex;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    error::Error,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const TARGETS: [Target; 5] = [
    Target {
        triple: "x86_64-pc-windows-msvc",
        platform: "windows",
        arch: "x64",
    },
    Target {
        triple: "x86_64-unknown-linux-gnu",
        platform: "linux",
        arch: "x64",
    },
    Target {
        triple: "aarch64-unknown-linux-gnu",
        platform: "linux",
        arch: "arm64",
    },
    Target {
        triple: "x86_64-apple-darwin",
        platform: "macos",
        arch: "x64",
    },
    Target {
        triple: "aarch64-apple-darwin",
        platform: "macos",
        arch: "arm64",
    },
];

#[derive(Clone, Copy)]
struct Target {
    triple: &'static str,
    platform: &'static str,
    arch: &'static str,
}

impl Target {
    fn find(triple: &str) -> Result<Self> {
        TARGETS
            .iter()
            .copied()
            .find(|t| t.triple == triple)
            .ok_or_else(|| format!("Unsupported target: {triple}").into())
    }

    fn name(self, version: &str) -> String {
        format!("xppen-firmware-v{version}-{}-{}", self.platform, self.arch)
    }

    fn archive_name(self, version: &str) -> String {
        format!(
            "{}{}",
            self.name(version),
            if self.platform == "linux" {
                ".tar.gz"
            } else {
                ".zip"
            }
        )
    }
}

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Dependency>,
    target_directory: PathBuf,
}

#[derive(Deserialize)]
struct Dependency {
    id: String,
    name: String,
    version: String,
    source: Option<String>,
    manifest_path: PathBuf,
    license: Option<String>,
    license_file: Option<PathBuf>,
}

fn main() {
    if let Err(error) = run(env::args().skip(1).collect()) {
        eprintln!("ERROR: {error}");
        std::process::exit(1);
    }
}

fn run(args: Vec<String>) -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("Missing repository root")?;
    let usage = "Usage: cargo xtask set-version VERSION | package VERSION --target TRIPLE [--output-dir PATH] | verify-release VERSION --folder PATH";
    let [action, version, rest @ ..] = args.as_slice() else {
        return Err(usage.into());
    };
    validate_version(version)?;
    match action.as_str() {
        "set-version" if rest.is_empty() => set_version(root, version),
        "package" => {
            let options = options(rest, &["--target", "--output-dir"])?;
            let target = Target::find(options.get("--target").ok_or("Missing --target")?)?;
            let output = options
                .get("--output-dir")
                .map(PathBuf::from)
                .unwrap_or(root.join("dist"));
            package::package(root, version, target, &output)
        }
        "verify-release" => {
            let options = options(rest, &["--folder"])?;
            verify_release(
                version,
                Path::new(options.get("--folder").ok_or("Missing --folder")?),
            )
        }
        _ => Err(usage.into()),
    }
}

fn options(args: &[String], allowed: &[&str]) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for pair in args.chunks(2) {
        let [key, value] = pair else {
            return Err("Option is missing its value".into());
        };
        if !allowed.contains(&key.as_str()) || result.insert(key.clone(), value.clone()).is_some() {
            return Err(format!("Unknown or duplicate option: {key}").into());
        }
    }
    Ok(result)
}

fn validate_version(version: &str) -> Result<()> {
    let parsed = semver::Version::parse(version)?;
    if !parsed.build.is_empty() || parsed.to_string() != version {
        return Err("Use a semantic version without v or build metadata, e.g. 1.2.3-beta.1".into());
    }
    Ok(())
}

/// Validate both edits before writing either file; preserve unrelated versions/line endings.
fn set_version(root: &Path, version: &str) -> Result<()> {
    validate_version(version)?;
    let edits = [
        ("Cargo.toml", r#"(?m)^(version = ")[^"\r\n]+("\r?$)"#),
        (
            "Cargo.lock",
            r#"(?m)(^name = "xppen-firmware"\r?\nversion = ")[^"\r\n]+("\r?$)"#,
        ),
    ];
    let mut updates = Vec::new();
    for (name, pattern) in edits {
        let path = root.join(name);
        let text = fs::read_to_string(&path)?;
        let regex = Regex::new(pattern)?;
        if regex.find_iter(&text).count() != 1 {
            return Err(format!("Expected exactly one app version in {name}").into());
        }
        let updated = regex.replace(&text, |m: &regex::Captures<'_>| {
            format!("{}{version}{}", &m[1], &m[2])
        });
        updates.push((path, updated.into_owned()));
    }
    for (path, text) in updates {
        fs::write(path, text)?;
    }
    println!("Application version: {version}");
    Ok(())
}

fn digest(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Pass arguments directly to the child process; version/path input never enters a shell.
fn command(root: &Path, program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "{program} failed ({}): {}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| "Path is not valid UTF-8".into())
}

fn relative_name(path: &Path, base: &Path) -> Result<String> {
    Ok(path_text(path.strip_prefix(base)?)?.replace('\\', "/"))
}

/// Rejects symlinks to keep package contents within the selected source tree.
fn files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(format!("Symlink in package source: {}", entry.path().display()).into());
        }
        if kind.is_dir() {
            result.extend(files(&entry.path())?);
        } else if kind.is_file() {
            result.push(entry.path());
        } else {
            return Err("Unsupported file type in package source".into());
        }
    }
    result.sort();
    Ok(result)
}

/// Cargo tree evaluates platform cfg and excludes dev dependencies correctly, unlike
/// inspecting metadata dep_kinds when rusb is both a Unix dependency and Windows test helper.
fn production_ids(metadata: &Metadata, tree: &str) -> Result<BTreeSet<String>> {
    let packages: BTreeMap<_, _> = metadata
        .packages
        .iter()
        .map(|p| ((p.name.as_str(), p.version.as_str()), p.id.as_str()))
        .collect();
    let regex = Regex::new(r"(?m)^(\S+) v(\S+)")?;
    let mut selected = BTreeSet::new();
    for row in regex.captures_iter(tree) {
        selected.insert(
            packages
                .get(&(row[1].as_ref(), row[2].as_ref()))
                .ok_or("Cannot match production tree to Cargo metadata")?
                .to_string(),
        );
    }
    if selected.is_empty() {
        return Err("Empty production dependency tree".into());
    }
    Ok(selected)
}

fn verify_release(version: &str, folder: &Path) -> Result<()> {
    validate_version(version)?;
    let mut expected = BTreeSet::new();
    for target in TARGETS {
        let name = target.archive_name(version);
        let sidecar = format!("{name}.sha256");
        let checksum = fs::read_to_string(folder.join(&sidecar))?;
        if checksum.trim() != format!("{}  {name}", digest(&folder.join(&name))?) {
            return Err(format!("Package checksum mismatch: {name}").into());
        }
        expected.extend([name, sidecar]);
    }
    let actual: BTreeSet<_> = fs::read_dir(folder)?
        .map(|entry| -> Result<_> {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                return Err("Non-file in release assets".into());
            }
            Ok(entry
                .file_name()
                .into_string()
                .map_err(|_| "Invalid filename")?)
        })
        .collect::<Result<_>>()?;
    if actual != expected {
        return Err("Unexpected files in release assets".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub(super) struct Temp(PathBuf);
    impl Temp {
        pub(super) fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = env::temp_dir().join(format!(
                "xppen-release-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        pub(super) fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn strict_versions() {
        for v in [
            "v1.2.3",
            "1.2",
            "01.2.3",
            "1.2.3-01",
            "1.2.3+meta",
            "../1.2.3",
            "1.2.3;echo bad",
        ] {
            assert!(validate_version(v).is_err(), "{v}");
        }
        for v in ["0.1.0", "1.2.3-beta.1", "1.2.3-0", "1.2.3-01a"] {
            validate_version(v).unwrap();
        }
    }

    #[test]
    fn version_changes_preserve_other_packages_and_crlf() {
        let temp = Temp::new();
        let manifest = "[package]\r\nname = \"xppen-firmware\"\r\nversion = \"0.1.0\"\r\n";
        let lock = "[[package]]\r\nname = \"xppen-firmware\"\r\nversion = \"0.1.0\"\r\n[[package]]\r\nname = \"dependency\"\r\nversion = \"9.8.7\"\r\n";
        fs::write(temp.path().join("Cargo.toml"), manifest).unwrap();
        fs::write(temp.path().join("Cargo.lock"), lock).unwrap();
        set_version(temp.path(), "1.2.3-beta.1").unwrap();
        let updated = fs::read_to_string(temp.path().join("Cargo.lock")).unwrap();
        assert_eq!(updated, lock.replacen("0.1.0", "1.2.3-beta.1", 1));
        assert!(set_version(temp.path(), "v2.0.0").is_err());
        assert_eq!(
            fs::read_to_string(temp.path().join("Cargo.lock")).unwrap(),
            updated
        );
    }

    #[test]
    fn malformed_second_manifest_does_not_change_first() {
        let temp = Temp::new();
        let manifest = "version = \"0.1.0\"\n";
        fs::write(temp.path().join("Cargo.toml"), manifest).unwrap();
        fs::write(temp.path().join("Cargo.lock"), "broken").unwrap();
        assert!(set_version(temp.path(), "1.0.0").is_err());
        assert_eq!(
            fs::read_to_string(temp.path().join("Cargo.toml")).unwrap(),
            manifest
        );
    }

    #[test]
    fn production_notices_exclude_test_and_packaging_dependencies() {
        let metadata: Metadata = serde_json::from_value(serde_json::json!({"target_directory":"target", "packages":
            (["app", "runtime", "test-helper", "release-tool"].map(|name| serde_json::json!({"name":name,"id":name,"version":"0.1.0","source":null,"manifest_path":"Cargo.toml","license":null,"license_file":null})))})).unwrap();
        assert_eq!(
            production_ids(
                &metadata,
                "app v0.1.0\nruntime v0.1.0\nruntime v0.1.0 (*)\n"
            )
            .unwrap(),
            BTreeSet::from(["app".into(), "runtime".into()])
        );
    }

    #[test]
    fn release_refuses_missing_corrupt_and_extra_packages() {
        let temp = Temp::new();
        assert!(verify_release("1.2.3", temp.path()).is_err());
        for target in TARGETS {
            let name = target.archive_name("1.2.3");
            let path = temp.path().join(&name);
            fs::write(&path, b"test").unwrap();
            fs::write(
                temp.path().join(format!("{name}.sha256")),
                format!("{}  {name}\n", digest(&path).unwrap()),
            )
            .unwrap();
        }
        verify_release("1.2.3", temp.path()).unwrap();
        let extra = temp.path().join("extra");
        fs::write(&extra, "extra").unwrap();
        assert!(verify_release("1.2.3", temp.path()).is_err());
        fs::remove_file(extra).unwrap();
        fs::write(
            temp.path().join(TARGETS[0].archive_name("1.2.3")),
            "corrupt",
        )
        .unwrap();
        assert!(verify_release("1.2.3", temp.path()).is_err());
    }
}
