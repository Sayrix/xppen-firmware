use super::*;

const CHOICES: [&str; 4] = [
    "stock-deco-fun-xs",
    "deco-fun-xs-no-smoothing",
    "deco-fun-xs-no-smoothing-full-pressure-gpio-reset-excitation-31",
    "deco-fun-xs-no-smoothing-pressure-every-3-cycles-gpio-reset-excitation-31",
];

/// Check native binary headers before labelling a build, including truncated PE input.
fn validate_architecture(path: &Path, target: Target) -> Result<()> {
    let mut data = Vec::new();
    File::open(path)?.take(4096).read_to_end(&mut data)?;
    let valid = match target.platform {
        "windows" => {
            let offset = data
                .get(60..64)
                .map(|v| u32::from_le_bytes(v.try_into().unwrap()) as usize);
            data.starts_with(b"MZ")
                && offset.is_some_and(|offset| {
                    offset
                        .checked_add(6)
                        .and_then(|end| data.get(offset..end))
                        .is_some_and(|v| v == b"PE\0\0\x64\x86")
                })
        }
        "linux" => {
            data.starts_with(b"\x7fELF\x02\x01")
                && data.get(18..20).is_some_and(|v| {
                    u16::from_le_bytes(v.try_into().unwrap())
                        == if target.arch == "x64" { 62 } else { 183 }
                })
        }
        "macos" => {
            data.starts_with(b"\xcf\xfa\xed\xfe")
                && data.get(4..8).is_some_and(|v| {
                    u32::from_le_bytes(v.try_into().unwrap())
                        == if target.arch == "x64" {
                            0x01000007
                        } else {
                            0x0100000C
                        }
                })
        }
        _ => false,
    };
    if !valid {
        return Err(format!("Executable does not match {}", target.triple).into());
    }
    Ok(())
}

/// Creates release archives with the platform archive utility.
/// macOS ditto preserves .app permissions/signatures; tar preserves Linux executable bits.
fn archive(root: &Path, folder: &Path, destination: &Path, platform: &str) -> Result<()> {
    if destination.exists() {
        return Err("Archive already exists".into());
    }
    let parent = folder.parent().ok_or("Missing package parent")?;
    let name = folder
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or("Invalid package name")?;
    let destination = path_text(destination)?;
    if platform == "macos" {
        command(
            root,
            "ditto",
            &["-c", "-k", "--keepParent", path_text(folder)?, destination],
        )?;
    } else {
        command(
            root,
            "tar",
            &[
                if platform == "windows" {
                    "-acf"
                } else {
                    "-czf"
                },
                destination,
                "-C",
                path_text(parent)?,
                name,
            ],
        )?;
    }
    Ok(())
}

fn executable_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    for path in files(source)? {
        let relative = path.strip_prefix(source)?;
        if relative.components().any(|c| {
            matches!(
                c.as_os_str().to_str(),
                Some(".cargo-ok" | ".cargo_vcs_info.json" | "target")
            )
        }) {
            continue;
        }
        let output = destination.join(relative);
        fs::create_dir_all(output.parent().ok_or("Missing output parent")?)?;
        fs::copy(path, output)?;
    }
    Ok(())
}

fn notices(root: &Path, metadata: &Metadata, destination: &Path, target: Target) -> Result<()> {
    fs::create_dir(destination)?;
    // App-scoped resolution excludes release-tool dependencies from notices.
    let tree = command(
        root,
        "cargo",
        &[
            "tree",
            "--package",
            "xppen-firmware",
            "--locked",
            "--offline",
            "--target",
            target.triple,
            "--edges",
            "normal,build",
            "--prefix",
            "none",
            "--format",
            "{p}",
            "--color",
            "never",
        ],
    )?;
    let resolved = production_ids(metadata, &tree)?;
    let license_name = Regex::new(r"(?i)^(LICENSE|LICENCE|COPYRIGHT|NOTICE|COPYING)([._-].*)?$")?;
    let mut packages: Vec<_> = metadata.packages.iter().collect();
    packages.sort_by_key(|p| (&p.name, &p.version));
    let mut inventory = Vec::new();
    for dependency in packages {
        if dependency.source.is_none() || !resolved.contains(&dependency.id) {
            continue;
        }
        let source = dependency
            .manifest_path
            .parent()
            .ok_or("Missing dependency source")?
            .canonicalize()?;
        let folder = destination.join(format!("{}-{}", dependency.name, dependency.version));
        let mut licenses = BTreeSet::new();
        for entry in fs::read_dir(&source)? {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && license_name.is_match(&entry.file_name().to_string_lossy())
            {
                licenses.insert(entry.path());
            }
        }
        if let Some(explicit) = &dependency.license_file {
            let path = source.join(explicit).canonicalize()?;
            if !path.starts_with(&source) {
                return Err("Dependency license escaped source directory".into());
            }
            licenses.insert(path);
        }
        if !licenses.is_empty() {
            fs::create_dir(&folder)?;
        }
        let mut names = Vec::new();
        for path in licenses {
            let name = path.file_name().ok_or("Missing license filename")?;
            fs::copy(&path, folder.join(name))?;
            names.push(name.to_str().ok_or("Invalid license filename")?.to_string());
        }
        if dependency.name == "libusb1-sys" {
            // Include exact LGPL source/build bindings for rebuilding and relinking.
            copy_tree(&source, &folder.join("source"))?;
        }
        inventory.push(
            serde_json::json!({"name":dependency.name,"version":dependency.version,
            "license":dependency.license,"source":dependency.source,"notices":names}),
        );
    }
    write_json(&destination.join("DEPENDENCIES.json"), &inventory)?;
    Ok(())
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(value)?))?;
    Ok(())
}

fn windows_runtime(root: &Path) -> Result<PathBuf> {
    let vswhere =
        PathBuf::from(env::var_os("ProgramFiles(x86)").ok_or("Missing ProgramFiles(x86)")?)
            .join("Microsoft Visual Studio/Installer/vswhere.exe");
    let installation = command(
        root,
        path_text(&vswhere)?,
        &[
            "-latest",
            "-products",
            "*",
            "-requires",
            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
            "-property",
            "installationPath",
        ],
    )?;
    if installation.trim().is_empty() {
        return Err("Missing Visual Studio installation".into());
    }
    let redist = Path::new(installation.trim()).join("VC/Redist/MSVC");
    let version_name = Regex::new(r"^\d+(?:\.\d+){2,3}$")?;
    let mut versions = Vec::new();
    for entry in fs::read_dir(redist)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_dir() && version_name.is_match(&name) {
            let version: Vec<u32> = name
                .split('.')
                .map(str::parse)
                .collect::<std::result::Result<_, _>>()?;
            versions.push((version, entry.path()));
        }
    }
    versions.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, version) in versions {
        let mut candidates = Vec::new();
        for entry in fs::read_dir(version.join("x64"))? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path().join("vcruntime140.dll");
            if name.starts_with("Microsoft.VC") && name.ends_with(".CRT") && path.is_file() {
                candidates.push(path);
            }
        }
        candidates.sort();
        if let Some(path) = candidates.into_iter().next() {
            return Ok(path);
        }
    }
    Err("Missing Visual Studio standard x64 runtime redistributable".into())
}

fn mac_bundle(root: &Path, executable: &Path, folder: &Path, version: &str) -> Result<()> {
    let app = folder.join("XPPen Firmware.app");
    let macos = app.join("Contents/MacOS");
    fs::create_dir_all(&macos)?;
    let application = macos.join("xppen-firmware");
    fs::copy(executable, &application)?;
    executable_permissions(&application)?;
    // Strict SemVer contains no XML metacharacters. Keep the bundle version numeric.
    let numeric = version.split('-').next().ok_or("Missing version")?;
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>io.github.sayrix.xppen-firmware</string>
<key>CFBundleName</key><string>XPPen Firmware</string>
<key>CFBundleDisplayName</key><string>XPPen Firmware</string>
<key>CFBundleExecutable</key><string>xppen-firmware</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>{numeric}</string>
<key>CFBundleVersion</key><string>{numeric}</string>
<key>LSMinimumSystemVersion</key><string>13.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
"#
    );
    fs::write(app.join("Contents/Info.plist"), plist)?;
    command(
        root,
        "codesign",
        &[
            "--force",
            "--sign",
            "-",
            "--timestamp=none",
            path_text(&app)?,
        ],
    )?;
    command(
        root,
        "codesign",
        &["--verify", "--strict", path_text(&app)?],
    )?;
    Ok(())
}

pub(super) fn package(root: &Path, version: &str, target: Target, output: &Path) -> Result<()> {
    validate_version(version)?;
    let manifest = fs::read_to_string(root.join("Cargo.toml"))?;
    let version_line = Regex::new(r#"(?m)^version = "([^"\r\n]+)"\r?$"#)?;
    let versions: Vec<_> = version_line.captures_iter(&manifest).collect();
    if versions.len() != 1 || &versions[0][1] != version {
        return Err("Version differs from Cargo.toml. Set version and rebuild first".into());
    }
    let metadata: Metadata = serde_json::from_str(&command(
        root,
        "cargo",
        &[
            "metadata",
            "--locked",
            "--offline",
            "--format-version",
            "1",
            "--filter-platform",
            target.triple,
        ],
    )?)?;
    let binary = if target.platform == "windows" {
        "xppen-firmware.exe"
    } else {
        "xppen-firmware"
    };
    let mut executable = metadata
        .target_directory
        .join(target.triple)
        .join("release")
        .join(binary);
    if !executable.is_file() {
        let host = command(root, "rustc", &["-vV"])?;
        if !host
            .lines()
            .any(|line| line == format!("host: {}", target.triple))
        {
            return Err("Missing native target release executable".into());
        }
        executable = metadata.target_directory.join("release").join(binary);
    }
    validate_architecture(&executable, target)?;
    if command(root, path_text(&executable)?, &["--version"])?.trim()
        != format!("xppen-firmware {version}")
    {
        return Err("Executable has a stale version; rebuild before packaging".into());
    }
    let output = if output.is_absolute() {
        output.to_path_buf()
    } else {
        env::current_dir()?.join(output)
    };
    let folder = output.join(target.name(version));
    let archive_path = output.join(target.archive_name(version));
    if folder.exists()
        || archive_path.exists()
        || output
            .join(format!("{}.sha256", target.archive_name(version)))
            .exists()
    {
        return Err("Package already exists; use a fresh output directory".into());
    }
    // Verify firmware before creating a partial package. Only these eight payloads are allowed.
    let checksum_text = fs::read_to_string(root.join("firmware/SHA256SUMS"))?;
    let checksums: BTreeMap<_, _> = checksum_text
        .lines()
        .map(|line| {
            line.split_once("  ")
                .map(|(hash, name)| (name, hash))
                .ok_or("Malformed firmware SHA256SUMS")
        })
        .collect::<std::result::Result<_, _>>()?;
    for choice in CHOICES {
        for extension in ["bin", "hex"] {
            let relative = format!("{choice}/firmware.{extension}");
            if Some(digest(&root.join("firmware").join(&relative))?.as_str())
                != checksums.get(relative.as_str()).copied()
            {
                return Err(format!("Firmware checksum differs: {relative}").into());
            }
        }
    }
    fs::create_dir_all(&output)?;
    fs::create_dir(&folder)?;
    for name in [
        "README.md",
        "LICENSE",
        "LICENSE-SCOPE.md",
        "THIRD_PARTY_NOTICES.md",
    ] {
        fs::copy(root.join(name), folder.join(name))?;
    }
    fs::copy(
        root.join("assets/Lato-OFL.txt"),
        folder.join("Lato-OFL.txt"),
    )?;
    if target.platform == "macos" {
        mac_bundle(root, &executable, &folder, version)?;
    } else {
        let application = folder.join(binary);
        fs::copy(&executable, &application)?;
        if target.platform == "windows" {
            fs::copy(windows_runtime(root)?, folder.join("vcruntime140.dll"))?;
        } else {
            executable_permissions(&application)?;
            fs::copy(
                root.join("assets/69-xppen-firmware.rules"),
                folder.join("69-xppen-firmware.rules"),
            )?;
        }
    }
    let firmware = folder.join("firmware");
    fs::create_dir(&firmware)?;
    for choice in CHOICES {
        let destination = firmware.join(choice);
        fs::create_dir(&destination)?;
        for extension in ["bin", "hex"] {
            let name = format!("firmware.{extension}");
            fs::copy(
                root.join("firmware").join(choice).join(&name),
                destination.join(&name),
            )?;
        }
    }
    for name in ["README.md", "SHA256SUMS"] {
        fs::copy(root.join("firmware").join(name), firmware.join(name))?;
    }
    notices(root, &metadata, &folder.join("third-party"), target)?;
    write_json(
        &folder.join("BUILD.json"),
        &serde_json::json!({"version":version,"target":target.triple,
        "source_commit":env::var("GITHUB_SHA").ok(), "cargo_lock_sha256":digest(&root.join("Cargo.lock"))?}),
    )?;
    let mut checksums = String::new();
    for path in files(&folder)? {
        checksums.push_str(&format!(
            "{}  {}\n",
            digest(&path)?,
            relative_name(&path, &folder)?
        ));
    }
    fs::write(folder.join("SHA256SUMS"), checksums)?;
    archive(root, &folder, &archive_path, target.platform)?;
    fs::write(
        output.join(format!("{}.sha256", target.archive_name(version))),
        format!(
            "{}  {}\n",
            digest(&archive_path)?,
            target.archive_name(version)
        ),
    )?;
    println!(
        "Portable folder: {}\nRelease archive: {}",
        folder.display(),
        archive_path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::Temp;

    #[test]
    fn rejects_wrong_and_truncated_binary_architectures() {
        let temp = Temp::new();
        let path = temp.path().join("app");
        for (triple, machine) in [
            ("x86_64-unknown-linux-gnu", 62u16),
            ("aarch64-unknown-linux-gnu", 183),
        ] {
            let mut data = [0; 64];
            data[..6].copy_from_slice(b"\x7fELF\x02\x01");
            data[18..20].copy_from_slice(&machine.to_le_bytes());
            fs::write(&path, data).unwrap();
            validate_architecture(&path, Target::find(triple).unwrap()).unwrap();
            for target in TARGETS {
                if target.triple != triple {
                    assert!(validate_architecture(&path, target).is_err());
                }
            }
        }
        for (triple, machine) in [
            ("x86_64-apple-darwin", 0x01000007u32),
            ("aarch64-apple-darwin", 0x0100000C),
        ] {
            let mut data = b"\xcf\xfa\xed\xfe".to_vec();
            data.extend(machine.to_le_bytes());
            fs::write(&path, data).unwrap();
            validate_architecture(&path, Target::find(triple).unwrap()).unwrap();
        }
        let mut data = [0; 256];
        data[..2].copy_from_slice(b"MZ");
        data[60..64].copy_from_slice(&128u32.to_le_bytes());
        data[128..134].copy_from_slice(b"PE\0\0\x64\x86");
        fs::write(&path, data).unwrap();
        validate_architecture(&path, TARGETS[0]).unwrap();
        data[60..64].copy_from_slice(&u32::MAX.to_le_bytes());
        fs::write(&path, data).unwrap();
        assert!(validate_architecture(&path, TARGETS[0]).is_err());
    }

    #[test]
    fn native_archive_roundtrip_and_executable_permissions() {
        let temp = Temp::new();
        let package = temp.path().join("package");
        fs::create_dir(&package).unwrap();
        let app = package.join("app");
        fs::write(&app, b"test").unwrap();
        executable_permissions(&app).unwrap();
        // Old registry timestamps must not break a ZIP package.
        File::options()
            .write(true)
            .open(&app)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH)
            .unwrap();
        let platform = if cfg!(windows) {
            "windows"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "linux"
        };
        let output = temp.path().join(if platform == "linux" {
            "app.tar.gz"
        } else {
            "app.zip"
        });
        archive(temp.path(), &package, &output, platform).unwrap();
        assert!(archive(temp.path(), &package, &output, platform).is_err());
        let extracted = temp.path().join("extracted");
        fs::create_dir(&extracted).unwrap();
        command(
            temp.path(),
            "tar",
            &[
                "-xf",
                path_text(&output).unwrap(),
                "-C",
                path_text(&extracted).unwrap(),
            ],
        )
        .unwrap();
        let copy = extracted.join("package/app");
        assert_eq!(fs::read(&copy).unwrap(), b"test");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(copy).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
    }
}
