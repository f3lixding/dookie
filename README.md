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
