use dookie_server_lib::{
    move_job, BundleClient, Config, Job, MainListener, MediaBundle, SpawnedJobType,
};
use std::{fs, os::unix::fs::symlink, path::Path, process::Output, time::Duration};
use tokio::{net::UnixListener, process::Command, sync::oneshot, time::timeout};

async fn run_cli(socket: &Path, args: &[&str]) -> Output {
    timeout(
        Duration::from_secs(10),
        Command::new(env!("CARGO_BIN_EXE_dookie_cli_client"))
            .arg("--socket")
            .arg(socket)
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("CLI should receive a response")
    .unwrap()
}

#[tokio::test]
async fn manual_move_transfers_fresh_media_and_reports_failures() {
    for fail in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let mut config = Config {
            move_job_period: 3600,
            age_threshold: u64::MAX,
            ..Config::default()
        };
        for library in ["movies", "shows"] {
            let src = root.join(".plex").join(library);
            let dst = root.join("storage").join(library);
            fs::create_dir_all(&src).unwrap();
            fs::create_dir_all(&dst).unwrap();
            config.move_map.insert(
                src.to_str().unwrap().to_owned().into(),
                dst.to_str().unwrap().to_owned().into(),
            );
        }
        let mut front_desk = None;
        let mut job =
            move_job::JobStruct::<BundleClient>::spawn_(&config, &mut front_desk).unwrap();
        let sender = job.give_sender().unwrap();
        // Wait for the startup scan before adding fresh files, so only the CLI can move them.
        timeout(Duration::from_secs(10), async {
            loop {
                let (tx, rx) = oneshot::channel();
                sender
                    .send((move_job::IncomingMessage::StatusRequest, Some(tx)))
                    .await
                    .unwrap();
                if matches!(
                    rx.await.unwrap(),
                    move_job::OutgoingMessage::TimeUntilNextScan(_)
                ) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        for library in ["movies", "shows"] {
            let src = root.join(".plex").join(library);
            let dst = root.join("storage").join(library);
            fs::create_dir_all(src.join("New title/Season 1")).unwrap();
            fs::write(src.join("New title/Season 1/video.mkv"), library).unwrap();
            fs::write(src.join("loose.mkv"), b"standalone video").unwrap();
            fs::write(dst.join("existing.mkv"), b"already moved").unwrap();
            symlink(dst.join("existing.mkv"), src.join("existing.mkv")).unwrap();
            symlink(dst.join("missing.mkv"), src.join("broken.mkv")).unwrap();
        }
        if fail {
            // One destination fails, but the other library must still be moved.
            fs::remove_dir_all(root.join("storage/movies")).unwrap();
            fs::write(root.join("storage/movies"), b"not a directory").unwrap();
        }

        let socket = root.join("cli.sock");
        let listener = MainListener::default()
            .assign_sender_bundle(MediaBundle::from_client_with_config(
                BundleClient::default(),
                &config,
            ))
            .assign_movejob_sender(sender.clone())
            .initiate_listener(UnixListener::bind(&socket).unwrap());

        for args in [&[][..], &["status"][..]] {
            let output = run_cli(&socket, args).await;
            assert!(output.status.success(), "{output:?}");
            assert!(String::from_utf8_lossy(&output.stdout).contains("Move job is idle"));
        }
        let output = run_cli(&socket, &["move"]).await;
        assert_eq!(output.status.success(), !fail, "{output:?}");
        if fail {
            assert!(String::from_utf8_lossy(&output.stderr).contains("Move failed"));
            assert_eq!(
                fs::read(root.join(".plex/movies/loose.mkv")).unwrap(),
                b"standalone video"
            );
            assert!(!fs::symlink_metadata(root.join(".plex/movies/loose.mkv"))
                .unwrap()
                .is_symlink());
        } else {
            assert!(String::from_utf8_lossy(&output.stdout).contains("Move completed successfully"));
            // Running it again skips both existing and newly created symlinks.
            assert!(run_cli(&socket, &["move"]).await.status.success());
        }
        for library in if fail {
            vec!["shows"]
        } else {
            vec!["movies", "shows"]
        } {
            let src = root.join(".plex").join(library);
            let dst = root.join("storage").join(library);
            assert_eq!(
                fs::read_link(src.join("New title")).unwrap(),
                dst.join("New title")
            );
            assert_eq!(
                fs::read(dst.join("New title/Season 1/video.mkv")).unwrap(),
                library.as_bytes()
            );
            assert_eq!(
                fs::read_link(src.join("loose.mkv")).unwrap(),
                dst.join("loose.mkv")
            );
            assert_eq!(
                fs::read(dst.join("loose.mkv")).unwrap(),
                b"standalone video"
            );
            assert_eq!(
                fs::read(src.join("existing.mkv")).unwrap(),
                b"already moved"
            );
            assert_eq!(
                fs::read_link(src.join("broken.mkv")).unwrap(),
                dst.join("missing.mkv")
            );
            assert!(!dst.join("missing.mkv").exists());
        }
        let (tx, rx) = oneshot::channel();
        sender
            .send((move_job::IncomingMessage::Stop, Some(tx)))
            .await
            .unwrap();
        assert_eq!(rx.await.unwrap(), move_job::OutgoingMessage::Ok);
        job.await.unwrap().unwrap();
        drop(listener);
    }
}
