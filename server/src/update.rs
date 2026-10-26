use flate2::read::GzDecoder;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    ffi::{CString, OsString},
    fs::{self, File},
    io::{self, Read},
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tempfile::TempDir;

type UpdateResult<T> = Result<T, Box<dyn Error + Send + Sync>>;
pub const SERVER_PATH: &str = "/usr/local/bin/dookie_server";
pub const RETRY_AFTER_ENV: &str = "DOOKIE_UPDATE_RETRY_AFTER";
const RELEASE_API: &str = "https://api.github.com/repos/f3lixding/dookie/releases/latest";
const DOWNLOAD_BASE: &str = "https://github.com/f3lixding/dookie/releases/download";
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_ARCHIVE: usize = 100 * 1024 * 1024;
const MAX_BINARY: u64 = 128 * 1024 * 1024;
const MAX_UNPACKED: u64 = 2 * MAX_BINARY + 2 * 1024 * 1024;
const BINARIES: [&str; 2] = ["dookie_server", "dookiec"];

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

impl Release {
    fn newer_version(&self, current: &Version) -> UpdateResult<Option<Version>> {
        if self.draft || self.prerelease {
            return Ok(None);
        }
        let version = Version::parse(
            self.tag_name
                .strip_prefix('v')
                .ok_or("Missing v tag prefix")?,
        )?;
        if !version.pre.is_empty() || !version.build.is_empty() || version <= *current {
            return Ok(None);
        }
        Ok(Some(version))
    }

    fn asset_url(&self, name: &str) -> UpdateResult<String> {
        let expected = format!("{DOWNLOAD_BASE}/{}/{name}", self.tag_name);
        let mut assets = self.assets.iter().filter(|asset| asset.name == name);
        let asset = assets.next().ok_or("Release asset is missing")?;
        if assets.next().is_some() || asset.browser_download_url != expected {
            return Err("Unexpected release asset URL or duplicate asset".into());
        }
        Ok(expected)
    }
}

async fn read_response(mut response: reqwest::Response, limit: usize) -> UpdateResult<Vec<u8>> {
    response.error_for_status_ref()?;
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err("Update download exceeds size limit".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > limit {
            return Err("Update download exceeds size limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Checks only this repository's published stable releases. A missing release is normal.
pub async fn check_for_update(
    install_dir: &Path,
    current: &Version,
) -> UpdateResult<Option<PreparedUpdate>> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("dookie/", env!("CARGO_PKG_VERSION")))
        .https_only(true)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()?;
    let response = client.get(RELEASE_API).send().await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let release: Release = serde_json::from_slice(&read_response(response, 1024 * 1024).await?)?;
    let Some(version) = release.newer_version(current)? else {
        return Ok(None);
    };
    let archive_name = format!("dookie-v{version}-x86_64-apple-darwin.tar.gz");
    let archive_url = release.asset_url(&archive_name)?;
    let checksum_url = release.asset_url(&format!("{archive_name}.sha256"))?;
    let checksum = read_response(client.get(checksum_url).send().await?, 4096).await?;
    let archive = read_response(client.get(archive_url).send().await?, MAX_ARCHIVE).await?;
    let install_dir = install_dir.to_path_buf();
    let update = tokio::task::spawn_blocking(move || {
        PreparedUpdate::from_archive(
            &install_dir,
            version,
            &archive,
            std::str::from_utf8(&checksum)?,
        )
    })
    .await??;
    Ok(Some(update))
}

/// Checks at startup and then once per day. Errors leave the running server untouched.
pub async fn wait_for_update(args: &[OsString]) -> PreparedUpdate {
    if !cfg!(all(target_os = "macos", target_arch = "x86_64"))
        || std::env::current_exe().ok().as_deref() != Some(Path::new(SERVER_PATH))
    {
        tracing::info!("Automatic updates require an Intel Mac installation at {SERVER_PATH}");
        return std::future::pending().await;
    }
    let current =
        Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo package version is valid");
    let install_dir = Path::new(SERVER_PATH).parent().unwrap();
    // A failed installation restarts the old server with a backoff, not a reboot loop.
    if let Some(retry_after) = std::env::var(RETRY_AFTER_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        tokio::time::sleep(Duration::from_secs(
            retry_after.saturating_sub(now).min(DAY.as_secs()),
        ))
        .await;
    }
    loop {
        match check_for_update(install_dir, &current).await {
            Ok(Some(update)) => match update.preflight(args).await {
                Ok(()) => return update,
                Err(error) => {
                    tracing::error!("Update preflight failed; keeping current version: {error}")
                }
            },
            Ok(None) => tracing::info!("No newer stable release available"),
            Err(error) => tracing::error!("Update check failed; retrying tomorrow: {error}"),
        }
        tokio::time::sleep(DAY).await;
    }
}

/// Both verified executables, staged on the installation filesystem for atomic renames.
pub struct PreparedUpdate {
    staging: TempDir,
    install_dir: PathBuf,
    version: Version,
}

impl PreparedUpdate {
    pub fn from_archive(
        install_dir: &Path,
        version: Version,
        bytes: &[u8],
        checksum: &str,
    ) -> UpdateResult<Self> {
        if bytes.len() > MAX_ARCHIVE {
            return Err("Release archive exceeds size limit".into());
        }
        let archive_name = format!("dookie-v{version}-x86_64-apple-darwin.tar.gz");
        let fields: Vec<_> = checksum.split_whitespace().collect();
        let digest = format!("{:x}", Sha256::digest(bytes));
        if fields.len() != 2 || fields[0] != digest || fields[1] != archive_name {
            return Err("Release checksum or archive name does not match".into());
        }
        // Bound all decompression before tar processes PAX/GNU metadata internally.
        let mut unpacked = Vec::new();
        GzDecoder::new(bytes)
            .take(MAX_UNPACKED + 1)
            .read_to_end(&mut unpacked)?;
        if unpacked.len() as u64 > MAX_UNPACKED {
            return Err("Unpacked release exceeds size limit".into());
        }
        let staging = tempfile::Builder::new()
            .prefix(".dookie-update-")
            .tempdir_in(install_dir)?;
        let mut archive = tar::Archive::new(unpacked.as_slice());
        let mut found = [false; 2];
        for (count, entry) in archive.entries()?.enumerate() {
            if count >= 4 {
                return Err("Too many entries in release archive".into());
            }
            let mut entry = entry?;
            let path = entry.path()?.into_owned();
            let index = match path.to_str() {
                Some("dookie/dookie_server") => 0,
                Some("dookie/dookie_cli_client") => 1,
                Some("dookie") | Some("dookie/")
                    if entry.header().entry_type().is_dir() && entry.size() == 0 =>
                {
                    continue
                }
                Some("dookie/README.md")
                    if entry.header().entry_type().is_file() && entry.size() <= 1024 * 1024 =>
                {
                    continue
                }
                _ => return Err(format!("Unexpected archive entry: {}", path.display()).into()),
            };
            if found[index] || !entry.header().entry_type().is_file() || entry.size() > MAX_BINARY {
                return Err("Duplicate, non-regular, or oversized executable in release".into());
            }
            let destination = staging.path().join(BINARIES[index]);
            let mut file = File::create(&destination)?;
            io::copy(&mut entry, &mut file)?;
            file.set_permissions(fs::Permissions::from_mode(0o755))?;
            file.sync_all()?;
            let mut header = [0; 8];
            File::open(&destination)?.read_exact(&mut header)?;
            // Thin, little-endian Mach-O 64, CPU_TYPE_X86_64 (the release workflow's target).
            if header != [0xcf, 0xfa, 0xed, 0xfe, 0x07, 0x00, 0x00, 0x01] {
                return Err("Release executable is not an Intel macOS binary".into());
            }
            found[index] = true;
        }
        if found != [true; 2] {
            return Err("Release must contain both server and CLI binaries".into());
        }
        Ok(Self {
            staging,
            install_dir: install_dir.to_path_buf(),
            version,
        })
    }

    /// Runs no background jobs: check the candidate's version and parse the existing config.
    pub async fn preflight(&self, args: &[OsString]) -> UpdateResult<()> {
        let binary = self.staging.path().join("dookie_server");
        let output = tokio::time::timeout(
            Duration::from_secs(30),
            tokio::process::Command::new(&binary)
                .arg("--version")
                .kill_on_drop(true)
                .output(),
        )
        .await??;
        if !output.status.success()
            || String::from_utf8_lossy(&output.stdout).trim()
                != format!("dookie_server {}", self.version)
        {
            return Err("Candidate version check failed".into());
        }
        let output = tokio::time::timeout(
            Duration::from_secs(30),
            tokio::process::Command::new(&binary)
                .args(args)
                .arg("--check-config")
                .kill_on_drop(true)
                .output(),
        )
        .await??;
        if !output.status.success() {
            return Err("Candidate cannot read the existing configuration".into());
        }
        Ok(())
    }

    /// Install both files, keeping one `_bak` generation. Failures restore the old pair.
    pub fn install(self) -> UpdateResult<InstalledUpdate> {
        for name in BINARIES {
            for path in [
                self.install_dir.join(name),
                self.install_dir.join(format!("{name}_bak")),
            ] {
                match fs::symlink_metadata(&path) {
                    Ok(metadata) if metadata.file_type().is_file() => {}
                    Ok(_) => {
                        return Err(format!(
                            "Refusing to replace non-regular file: {}",
                            path.display()
                        )
                        .into())
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        let mut installed = InstalledUpdate {
            server: self.install_dir.join("dookie_server"),
            replacements: Vec::new(),
        };
        let result: UpdateResult<()> = (|| {
            for name in BINARIES {
                let destination = self.install_dir.join(name);
                let backup = if destination.exists() {
                    let backup = self.install_dir.join(format!("{name}_bak"));
                    fs::rename(&destination, &backup)?;
                    Some(backup)
                } else {
                    None
                };
                installed.replacements.push((destination.clone(), backup));
                fs::rename(self.staging.path().join(name), &destination)?;
            }
            self.staging.close()?;
            File::open(&self.install_dir)?.sync_all()?;
            Ok(())
        })();
        if let Err(error) = result {
            installed.rollback()?;
            return Err(error);
        }
        Ok(installed)
    }
}

/// A transaction guard: until exec succeeds, dropping it restores the original files.
pub struct InstalledUpdate {
    server: PathBuf,
    replacements: Vec<(PathBuf, Option<PathBuf>)>,
}

#[derive(Debug)]
pub struct RollbackIncomplete(Vec<String>);

impl std::fmt::Display for RollbackIncomplete {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Update rollback incomplete; restore _bak files manually: {}",
            self.0.join("; ")
        )
    }
}

impl Error for RollbackIncomplete {}

impl InstalledUpdate {
    fn rollback(&mut self) -> UpdateResult<()> {
        let mut failures = Vec::new();
        for (destination, backup) in std::mem::take(&mut self.replacements).into_iter().rev() {
            let result = match &backup {
                Some(backup) => fs::rename(backup, &destination),
                None => match fs::remove_file(&destination) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(error),
                },
            };
            if let Err(error) = result {
                failures.push(format!("{}: {error}", destination.display()));
                self.replacements.push((destination, backup));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(Box::new(RollbackIncomplete(failures)))
        }
    }

    /// Replaces this process, preserving its PID, arguments, environment, and working directory.
    /// CLOEXEC closes the old listener and its lock before the new server reclaims the socket.
    pub fn exec(mut self, args: &[OsString]) -> UpdateResult<()> {
        let path = CString::new(self.server.as_os_str().as_bytes())?;
        let mut arguments = vec![path.clone()];
        for arg in args {
            arguments.push(CString::new(arg.as_bytes())?);
        }
        let mut argv: Vec<_> = arguments.iter().map(|arg| arg.as_ptr()).collect();
        argv.push(std::ptr::null());
        // Use execv, not execvp: execvp can run an invalid executable as a shell script
        // instead of returning ENOEXEC, preventing rollback. All C strings live through
        // this call and argv is NUL-terminated; execv inherits the current environment.
        unsafe {
            libc::execv(path.as_ptr(), argv.as_ptr());
        }
        let error = io::Error::last_os_error();
        self.rollback()?;
        Err(error.into())
    }
}

impl Drop for InstalledUpdate {
    fn drop(&mut self) {
        if let Err(error) = self.rollback() {
            eprintln!("Unable to roll back update; restore the _bak binaries manually: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};
    use std::process::Command;

    const SERVER: &[u8] = b"\xcf\xfa\xed\xfe\x07\x00\x00\x01new server";
    const CLIENT: &[u8] = b"\xcf\xfa\xed\xfe\x07\x00\x00\x01new client";

    fn archive(entries: &[(&str, tar::EntryType, &[u8])]) -> (Vec<u8>, String) {
        let mut archive = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
        for (path, kind, data) in entries {
            let mut header = tar::Header::new_gnu();
            // Raw names let the rejection test construct malicious traversal entries too.
            header.as_mut_bytes()[..path.len()].copy_from_slice(path.as_bytes());
            header.set_entry_type(*kind);
            header.set_mode(0o755);
            header.set_size(data.len() as u64);
            header.set_cksum();
            archive.append(&header, *data).unwrap();
        }
        let bytes = archive.into_inner().unwrap().finish().unwrap();
        let checksum = format!(
            "{:x}  dookie-v1.2.3-x86_64-apple-darwin.tar.gz\n",
            Sha256::digest(&bytes)
        );
        (bytes, checksum)
    }

    fn original_installation() -> TempDir {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("dookie_server"), b"old server").unwrap();
        fs::write(directory.path().join("dookiec"), b"old client").unwrap();
        directory
    }

    fn prepare(directory: &Path) -> PreparedUpdate {
        let (bytes, checksum) = archive(&[
            ("dookie/", tar::EntryType::Directory, b""),
            ("dookie/README.md", tar::EntryType::Regular, b"readme"),
            ("dookie/dookie_server", tar::EntryType::Regular, SERVER),
            ("dookie/dookie_cli_client", tar::EntryType::Regular, CLIENT),
        ]);
        PreparedUpdate::from_archive(directory, Version::new(1, 2, 3), &bytes, &checksum).unwrap()
    }

    fn assert_original(directory: &Path) {
        assert_eq!(
            fs::read(directory.join("dookie_server")).unwrap(),
            b"old server"
        );
        assert_eq!(fs::read(directory.join("dookiec")).unwrap(), b"old client");
    }

    #[test]
    fn installs_both_executables_with_backups_and_rolls_back_if_not_executed() {
        let directory = original_installation();
        fs::write(directory.path().join("dookie_server_bak"), b"older server").unwrap();
        let installed = prepare(directory.path()).install().unwrap();
        for (name, new, old) in [
            ("dookie_server", SERVER, b"old server".as_slice()),
            ("dookiec", CLIENT, b"old client".as_slice()),
        ] {
            assert_eq!(fs::read(directory.path().join(name)).unwrap(), new);
            assert_eq!(
                fs::read(directory.path().join(format!("{name}_bak"))).unwrap(),
                old
            );
            assert_eq!(
                fs::metadata(directory.path().join(name))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
        }
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 4);
        drop(installed);
        assert_original(directory.path());
    }

    #[test]
    fn rejects_bad_checksums_and_unsafe_archives_without_touching_installed_files() {
        let directory = original_installation();
        let good = [
            ("dookie/dookie_server", tar::EntryType::Regular, SERVER),
            ("dookie/dookie_cli_client", tar::EntryType::Regular, CLIENT),
        ];
        let (bytes, checksum) = archive(&good);
        for bad_checksum in [
            format!(
                "{}  dookie-v1.2.3-x86_64-apple-darwin.tar.gz",
                "0".repeat(64)
            ),
            checksum.replace("v1.2.3", "v9.9.9"),
        ] {
            assert!(PreparedUpdate::from_archive(
                directory.path(),
                Version::new(1, 2, 3),
                &bytes,
                &bad_checksum
            )
            .is_err());
        }
        let mut cases = vec![good[..1].to_vec()];
        for entry in [
            ("dookie/dookie_server", tar::EntryType::Regular, SERVER),
            ("../escaped", tar::EntryType::Regular, SERVER),
            ("/tmp/escaped", tar::EntryType::Regular, SERVER),
            (
                "dookie/dookie_cli_client",
                tar::EntryType::Symlink,
                b"".as_slice(),
            ),
        ] {
            let mut entries = good.to_vec();
            entries.push(entry);
            cases.push(entries);
        }
        cases.push(vec![
            (
                "dookie/dookie_server",
                tar::EntryType::Regular,
                b"not Mach-O",
            ),
            good[1],
        ]);
        for entries in cases {
            let (bytes, checksum) = archive(&entries);
            assert!(PreparedUpdate::from_archive(
                directory.path(),
                Version::new(1, 2, 3),
                &bytes,
                &checksum
            )
            .is_err());
            assert_original(directory.path());
            assert_eq!(
                fs::read_dir(directory.path()).unwrap().count(),
                2,
                "no staged files survive rejection"
            );
        }
    }

    #[test]
    fn partial_install_and_failed_exec_restore_both_binaries() {
        let directory = original_installation();
        let prepared = prepare(directory.path());
        // Simulate a staged file disappearing after validation but before the second rename.
        fs::remove_file(prepared.staging.path().join("dookiec")).unwrap();
        assert!(prepared.install().is_err());
        assert_original(directory.path());

        let installed = prepare(directory.path()).install().unwrap();
        // The tiny test Mach-O header is not executable on either Linux or macOS.
        assert!(installed.exec(&[]).is_err());
        assert_original(directory.path());

        fs::remove_file(directory.path().join("dookiec")).unwrap();
        let installed = prepare(directory.path()).install().unwrap();
        assert!(installed.exec(&[]).is_err());
        assert!(
            !directory.path().join("dookiec").exists(),
            "rollback removes a newly installed client"
        );
        assert_eq!(
            fs::read(directory.path().join("dookie_server")).unwrap(),
            b"old server"
        );
    }

    #[test]
    fn incomplete_rollback_still_restores_other_binary() {
        let directory = original_installation();
        let installed = prepare(directory.path()).install().unwrap();
        let client = directory.path().join("dookiec");
        fs::remove_file(&client).unwrap();
        fs::create_dir(&client).unwrap();
        let error = installed.exec(&[]).unwrap_err();
        assert!(error.is::<RollbackIncomplete>());
        assert_eq!(
            fs::read(directory.path().join("dookie_server")).unwrap(),
            b"old server"
        );
        assert_eq!(
            fs::read(directory.path().join("dookiec_bak")).unwrap(),
            b"old client"
        );
        assert!(client.is_dir());
    }

    #[test]
    fn rejects_oversized_compressed_metadata_before_tar_parsing() {
        let directory = original_installation();
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::GNULongName);
        header.set_size(MAX_UNPACKED + 1);
        header.set_cksum();
        std::io::Write::write_all(&mut encoder, header.as_bytes()).unwrap();
        io::copy(&mut io::repeat(b'x').take(MAX_UNPACKED + 1), &mut encoder).unwrap();
        let bytes = encoder.finish().unwrap();
        let checksum = format!(
            "{:x}  dookie-v1.2.3-x86_64-apple-darwin.tar.gz",
            Sha256::digest(&bytes)
        );
        let error = PreparedUpdate::from_archive(
            directory.path(),
            Version::new(1, 2, 3),
            &bytes,
            &checksum,
        )
        .err()
        .expect("oversized metadata must be rejected");
        assert_eq!(error.to_string(), "Unpacked release exceeds size limit");
        assert_original(directory.path());
    }

    #[test]
    fn refuses_symlink_destinations_before_replacing_either_binary() {
        let directory = original_installation();
        fs::remove_file(directory.path().join("dookiec")).unwrap();
        std::os::unix::fs::symlink("dookie_server", directory.path().join("dookiec")).unwrap();
        assert!(prepare(directory.path()).install().is_err());
        assert_eq!(
            fs::read(directory.path().join("dookie_server")).unwrap(),
            b"old server"
        );
        assert!(fs::symlink_metadata(directory.path().join("dookiec"))
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[tokio::test]
    #[serial_test::serial(socket_lock_lifetime)]
    async fn preflight_requires_matching_version_and_valid_config_arguments() {
        let directory = original_installation();
        for (version, config_status, expected) in
            [("1.2.3", 0, true), ("1.2.2", 0, false), ("1.2.3", 7, false)]
        {
            let staging = tempfile::tempdir_in(directory.path()).unwrap();
            let path = staging.path().join("dookie_server");
            fs::write(&path, format!(
                "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'dookie_server {version}'; exit 0; fi\n[ \"$1\" = --config_path ] && [ \"$2\" = 'config with spaces.yaml' ] && [ \"$3\" = --check-config ] || exit 9\nexit {config_status}\n"
            )).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            let prepared = PreparedUpdate {
                staging,
                install_dir: directory.path().to_path_buf(),
                version: Version::new(1, 2, 3),
            };
            let result = prepared
                .preflight(&["--config_path".into(), "config with spaces.yaml".into()])
                .await;
            assert_eq!(result.is_ok(), expected);
            assert_original(directory.path());
        }
    }

    #[test]
    #[serial_test::serial(socket_lock_lifetime)]
    fn exec_preserves_pid_arguments_environment_and_releases_socket() {
        if let Some(directory) = std::env::var_os("DOOKIE_EXEC_TEST_DIR") {
            let directory = PathBuf::from(directory).canonicalize().unwrap();
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let _entered = runtime.enter();
            let (_socket, _lock) = crate::bind_cli_socket(&directory.join("cli.sock")).unwrap();
            fs::write(directory.join("before_pid"), std::process::id().to_string()).unwrap();
            let staging = tempfile::tempdir_in(&directory).unwrap();
            fs::copy(
                std::env::current_exe().unwrap(),
                staging.path().join("dookie_server"),
            )
            .unwrap();
            fs::write(staging.path().join("dookiec"), b"replacement client").unwrap();
            let prepared = PreparedUpdate {
                staging,
                install_dir: directory,
                version: Version::new(1, 2, 3),
            };
            prepared
                .install()
                .unwrap()
                .exec(&[
                    "--exact".into(),
                    "update::tests::replacement_process_probe".into(),
                    "--nocapture".into(),
                ])
                .unwrap();
            unreachable!("exec should replace this process");
        }
        let directory = original_installation();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "update::tests::exec_preserves_pid_arguments_environment_and_releases_socket",
                "--nocapture",
            ])
            .env("DOOKIE_EXEC_TEST_DIR", directory.path())
            .env("DOOKIE_EXEC_TEST_VALUE", "preserved")
            .current_dir(directory.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read(directory.path().join("before_pid")).unwrap(),
            fs::read(directory.path().join("after_pid")).unwrap()
        );
        assert_eq!(
            fs::read(directory.path().join("dookie_server_bak")).unwrap(),
            b"old server"
        );
        assert_eq!(
            fs::read(directory.path().join("dookiec_bak")).unwrap(),
            b"old client"
        );
        assert_eq!(
            fs::read(directory.path().join("dookiec")).unwrap(),
            b"replacement client"
        );
    }

    #[test]
    fn replacement_process_probe() {
        let Some(directory) = std::env::var_os("DOOKIE_EXEC_TEST_DIR") else {
            return;
        };
        let directory = PathBuf::from(directory).canonicalize().unwrap();
        assert_eq!(std::env::current_dir().unwrap(), directory);
        assert_eq!(
            std::env::var("DOOKIE_EXEC_TEST_VALUE").unwrap(),
            "preserved"
        );
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let path = directory.join("cli.sock");
            let (listener, _lock) = crate::bind_cli_socket(&path).unwrap();
            let _client = tokio::net::UnixStream::connect(&path).await.unwrap();
            listener.accept().await.unwrap();
        });
        fs::write(directory.join("after_pid"), std::process::id().to_string()).unwrap();
    }
}
