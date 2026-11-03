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

async fn wait_for_idle(socket: &Path) {
    timeout(Duration::from_secs(10), async {
        loop {
            let output = run_cli(socket, &["status"]).await;
            assert!(output.status.success(), "{output:?}");
            if String::from_utf8_lossy(&output.stdout).contains("Move job is idle") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("background move should finish");
}

#[tokio::test]
async fn manual_move_returns_before_copying_and_keeps_status_responsive() {
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

        // Block a real copy at its destination until the CLI has returned and we
        // have queried status and tried a duplicate start. No large files or timing guesses.
        let pipe = root.join("storage/shows/loose.mkv");
        let pipe_name = std::ffi::CString::new(pipe.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: pipe_name is a valid NUL-terminated path in our temporary directory.
        assert_eq!(unsafe { libc::mkfifo(pipe_name.as_ptr(), 0o600) }, 0);
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let reader = std::thread::spawn(move || {
            // A test failure drops release too, unblocking this reader and the copy
            // so the Tokio runtime can still shut down.
            let _ = wait.recv_timeout(Duration::from_secs(10));
            let bytes = fs::read(&pipe).unwrap();
            fs::remove_file(&pipe).unwrap();
            fs::write(&pipe, &bytes).unwrap();
            bytes
        });

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
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("Move job started"));
        let output = run_cli(&socket, &["status"]).await;
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("Move job is already running"));
        let output = run_cli(&socket, &["move"]).await;
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("no new move started"));
        release.send(()).unwrap();
        assert_eq!(reader.join().unwrap(), b"standalone video");
        wait_for_idle(&socket).await;
        if fail {
            assert_eq!(
                fs::read(root.join(".plex/movies/loose.mkv")).unwrap(),
                b"standalone video"
            );
            assert!(!fs::symlink_metadata(root.join(".plex/movies/loose.mkv"))
                .unwrap()
                .is_symlink());
        } else {
            // Running it again skips both existing and newly created symlinks.
            let output = run_cli(&socket, &["move"]).await;
            assert!(output.status.success(), "{output:?}");
            assert!(String::from_utf8_lossy(&output.stdout).contains("Move job started"));
            wait_for_idle(&socket).await;
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

#[tokio::test]
async fn disconnected_client_does_not_stop_the_listener() {
    use dookie_server_lib::{envelope, DataType, Envelope, MoveJobCommand, MoveJobRequest};
    use prost::Message;
    use tokio::{io::AsyncWriteExt, net::UnixStream, sync::mpsc};

    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("cli.sock");
    let (sender, mut requests) = mpsc::channel(10);
    let _listener = MainListener::default()
        .assign_sender_bundle(MediaBundle::from_client_with_config(
            BundleClient::default(),
            &Config::default(),
        ))
        .assign_movejob_sender(sender)
        .initiate_listener(UnixListener::bind(&socket).unwrap());

    let mut stream = UnixStream::connect(&socket).await.unwrap();
    stream
        .write_all(
            &Envelope {
                data_type: DataType::Movejobrequest as i32,
                data: Some(envelope::Data::MoveJobRequest(MoveJobRequest {
                    command: MoveJobCommand::Startjob as i32,
                })),
            }
            .encode_to_vec(),
        )
        .await
        .unwrap();
    let (request, reply) = requests.recv().await.unwrap();
    assert_eq!(request, move_job::IncomingMessage::Start);
    // Close after the request is accepted but before its response is sent.
    drop(stream);
    reply.unwrap().send(move_job::OutgoingMessage::Ok).unwrap();

    let respond = async {
        let (request, reply) = requests.recv().await.unwrap();
        assert_eq!(request, move_job::IncomingMessage::StatusRequest);
        reply
            .unwrap()
            .send(move_job::OutgoingMessage::TimeUntilNextScan(123))
            .unwrap();
    };
    let (output, ()) = tokio::join!(run_cli(&socket, &["status"]), respond);
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("next run in 123 seconds"));
}
