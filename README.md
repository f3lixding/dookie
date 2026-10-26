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
`macos-15-intel` for pull requests and pushes to `master`. The workflow pins Rust
1.90.0, uses `Cargo.lock`, and installs `protoc` through Homebrew. It does not use
the Linux-only Nix flake.

The current deployment target is **macOS 15.0 on Intel (`x86_64`)**. Older macOS
versions are not validated. Builds check the architecture and reject dependencies
on non-system dynamic libraries. The server's `--help` is smoke-tested; the demo
CLI requires a running server at `/tmp/dookie.sock`, so it is not launched in CI.

After a successful run, open **Actions → macOS build → the run → Artifacts** and
download `dookie-x86_64-apple-darwin`. GitHub wraps the artifact in a ZIP; extract
it, then extract the enclosed `.tar.gz` to preserve the binaries' executable bits.
The `dookie/` directory contains both binaries and this README. These test builds
are retained for 14 days, not published as GitHub Releases.

Builds are not Developer ID signed or notarized, and macOS may require approval
to run downloaded binaries. Configuration, credentials, automatic installation,
and daemon startup are not included. To run the server with an existing config:

```sh
./dookie/dookie_server --config_path /absolute/path/to/config.yaml
```
