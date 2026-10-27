## What is this?
This is a daemon, a web application server and everything I can think of to run on my 2018 macbook pro 13 that I have lying around. I mainly use it to manage my Plex server and everything around it.
It's also just an excuse for me to write crappy rust.

## What does it do now?
Currently it does the following:
- Moves files from their temporary location to the specified permanent location after specified amount of time
- Ensures that it is safe to call scans on Plex by checking to see if there are any broken symlinks. If it is safe, calls for Plex to refresh it library.
- Terminates any ongoing items in the torrent client (i.e. download and upload) when there are ongoing remote sessions. This also includes intercepting and caching requests issued by index managers (this is a WIP). 

In the future, I want it to also do the following:
- Curates a list of files to delete based on specified criteria.
- Deletes them when it's time (with the way I had set things up this means a bit more than just rm file).
- Resumes any ongoing items in the torrent client when no remote sessions are detected.

## Discord operations

- `/docker_ps` lists running containers, like `docker container ls`. Long listings are attached as a text file.
- `/restart_container container_id:<ID or name>` runs `docker container restart` for one container (for example, qBittorrent).
- `/restart_services` shuts down and reopens the native macOS Plex Media Server, Sonarr, Radarr, and Prowlarr apps. It sends SIGTERM, waits up to 30 seconds for each app to exit, then runs `open /Applications/<app>.app`. If an app is already stopped it is opened; if shutdown fails it is not opened again. Each app's outcome is reported separately, and failures do not prevent the remaining apps from being attempted. Reopening confirms launch, not API readiness.
- `/help` lists the registered commands and their arguments, including the media search/add and access commands. Admin-only commands are shown only to administrators.

The three operational commands require Discord Administrator permission in the configured guild. Their replies and help are private. The bot must run as the macOS user who owns the apps and have `docker`, `pkill`, `pgrep`, and `open` on its PATH, with access to Docker Desktop. Restarts interrupt active streams or downloads; these commands do not use sudo or force-kill a service that refuses to shut down.

## Intel macOS builds

GitHub Actions builds and tests both `dookie_server` and `dookie_cli_client` on
`macos-15-intel` for pull requests, pushes to `master`, and version tags. The
workflow pins Rust 1.90.0, uses `Cargo.lock`, and installs `protoc` through
Homebrew. It does not use the Linux-only Nix flake.

The current deployment target is **macOS 15.0 on Intel (`x86_64`)**. Older macOS
versions are not validated. Builds check the architecture and reject dependencies
on non-system dynamic libraries. The server's `--help` is smoke-tested; the demo
CLI requires a running server at `/tmp/dookie.sock`, so it is not launched in CI.

After a successful run, open **Actions → macOS build and release → the run →
Artifacts** and download `dookie-x86_64-apple-darwin`. GitHub wraps the artifact in
a ZIP; extract it, then extract the enclosed `.tar.gz` to preserve the binaries'
executable bits. The `dookie/` directory contains both binaries and this README.
PR and branch builds use `build-<commit>` in the archive name. Actions artifacts
are retained for 14 days; only version tags create GitHub Releases.

Builds are not Developer ID signed or notarized, and macOS may require approval
to run downloaded binaries. Configuration, credentials, initial installation,
and service-manager setup are not included. To run the server with an existing config:

```sh
./dookie/dookie_server --config_path /absolute/path/to/config.yaml
```

## Making a release

1. Set the same version in `server/Cargo.toml` and `cli_client/Cargo.toml`, update
   `Cargo.lock` with `cargo check --workspace`, and merge the change to `master`.
2. Tag that commit with `vMAJOR.MINOR.PATCH` and push the tag. For example, for
   packages at version `0.1.0`:

   ```sh
   git tag -a v0.1.0 -m "Release v0.1.0"
   git push origin v0.1.0
   ```

3. The workflow validates that the tag matches both package versions, then uses
   the same build, test, and packaging steps as PR builds. Prerelease tags are not
   supported yet. A failed build or test prevents release creation.
4. Open **Releases** and review the new draft, its generated release notes, and
   its two assets: `dookie-v0.1.0-x86_64-apple-darwin.tar.gz` and the corresponding
   `.tar.gz.sha256` file. Publish the draft when ready. These release assets are
   separate from expiring Actions artifacts and remain until deleted.

Download both assets into the same directory, verify the checksum, then extract:

```sh
shasum -a 256 -c dookie-v0.1.0-x86_64-apple-darwin.tar.gz.sha256
tar -xzf dookie-v0.1.0-x86_64-apple-darwin.tar.gz
```

The release job uses GitHub's built-in token with `contents: write`; PR builds
have read-only repository permissions and cannot create releases. No personal
access token or external storage is needed. The workflow does not overwrite an
existing release: if an upload fails after draft creation, inspect the draft
before retrying and explicitly remove an incomplete draft if appropriate. Do not
move a published version tag to different source code.

## Automatic updates and restarts

On Intel macOS, a server installed as a regular executable at
`/usr/local/bin/dookie_server` checks this repository's **latest published stable
GitHub Release** at startup and every 24 hours thereafter. Drafts, prereleases,
equal versions, and downgrades are not installed. Running from another path or
on another platform disables automatic updates; `--no-auto-update` disables them
explicitly. The first installation of a version containing the updater is manual.

The daemon's user needs write access to `/usr/local/bin` and its installed files;
it never invokes `sudo`. The archive's `dookie_cli_client` is installed as
`/usr/local/bin/dookiec`. The release asset names stay unchanged.

An update verifies the SHA-256 checksum and Intel Mach-O binaries, stages both
executables on the installation filesystem, and runs the new server's `--version`
and `--check-config` against the current arguments. Only then does it wait for the
move job to finish its current batch and stop. It renames the old executables to
`dookie_server_bak` and `dookiec_bak`, installs the new pair, and uses Unix `exec`
to replace itself. The PID, command-line arguments, environment, and working
directory are preserved; there is no period with two daemons running jobs.

The old socket and lock descriptors close on `exec`. Startup claims
`/tmp/dookie.sock.lock` before starting any jobs and reclaims `/tmp/dookie.sock`
only if it is a stale socket. On macOS, reclamation also requires the ownership
record written by this version: a socket left by an older daemon may need one
final manual removal **after stopping that daemon**. Subsequent restarts recover
automatically. Live servers, regular files, and symlinks are preserved. Do not
delete the lock file; it holds the ownership record and coordinates instances.

Download, checksum, and preflight failures leave the current server untouched.
Installation or `exec` failures roll back the binaries; after stopping the move
job, the old server is restarted with a one-day update backoff. If filesystem
errors prevent complete rollback, the daemon exits with a recovery error rather
than starting a mismatched pair; restore the retained backups manually. Backups retain
one generation. They are not a health-monitoring system: a crash after a successful
`exec`, or a power loss between filesystem operations, can still require manual
recovery. Stop the daemon, restore the `_bak` files, and restart with
`--no-auto-update` while investigating a bad release. SHA-256 detects corruption;
the updater trusts the repository's release publishers, not a separate signing key.

`dookie_server --config_path /path/to/config.yaml --check-config` validates YAML
without opening the socket or starting background jobs.
