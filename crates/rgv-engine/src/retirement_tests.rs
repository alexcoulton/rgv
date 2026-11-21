use crate::{
    AlignmentSceneEngine, LocalAlignmentSceneEngineConfig, ReferenceEngine, TrackEngine, TrackSpec,
    TrackSpecKind,
};
use rgv_alignment_scene::AllocationLedger;
use rgv_core::TrackId;
use rgv_hts::{AlignmentOpenOptions, QueryOptions};
use std::{num::NonZeroUsize, sync::mpsc, thread, time::Duration};
use tiny_http::{Response, Server, StatusCode};

#[test]
fn retirement_returns_before_blocked_io_and_abandons_queued_opens() {
    for reference in [false, true] {
        let server = Server::http("127.0.0.1:0").unwrap();
        let path = format!("http://{}/blocked.fa", server.server_addr());
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server_worker = thread::spawn(move || {
            let request = server.recv().unwrap();
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            request.respond(Response::empty(StatusCode(404))).unwrap();
            server
        });
        let retire: Box<dyn FnOnce() -> Option<thread::JoinHandle<()>> + Send> = if reference {
            let engine = ReferenceEngine::spawn(4);
            engine.try_open(path.clone().into()).unwrap();
            engine.try_open(path.into()).unwrap();
            Box::new(move || engine.retire())
        } else {
            let spec = TrackSpec {
                id: TrackId::new(1),
                name: "blocked".into(),
                path: path.into(),
                kind: TrackSpecKind::BigWig,
            };
            let engine = TrackEngine::spawn(vec![spec.clone(), spec.clone()], 4);
            engine.try_add(spec).unwrap();
            Box::new(move || engine.retire())
        };
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let (retired_tx, retired_rx) = mpsc::channel();
        let retire_caller = thread::spawn(move || retired_tx.send(retire()).unwrap());
        let retired = retired_rx.recv_timeout(Duration::from_secs(2));
        // Always unblock IO, including when a regression makes retirement join.
        release_tx.send(()).unwrap();
        let worker = retired
            .expect("retirement waited for blocked source IO")
            .unwrap();
        retire_caller.join().unwrap();
        let server = server_worker.join().unwrap();
        worker.join().unwrap();
        assert!(
            server.try_recv().unwrap().is_none(),
            "obsolete queued opens reached the server"
        );
    }
}

/// Starts an engine whose worker is blocked inside a source read, and returns the closure which
/// drops it. `kind` selects the scene, track, or reference engine.
fn engine_blocked_on(kind: &str, url: String) -> Box<dyn FnOnce() + Send> {
    match kind {
        "scene" => {
            let capacity = NonZeroUsize::new(4).unwrap();
            let engine = AlignmentSceneEngine::spawn_local_bam(LocalAlignmentSceneEngineConfig {
                path: url.into(),
                open_options: AlignmentOpenOptions::default(),
                query_options: QueryOptions::default(),
                ledger: AllocationLedger::new(1 << 20, 0),
                mailbox_control_capacity: capacity,
                source_control_capacity: capacity,
                maximum_source_control_burst: capacity,
            });
            Box::new(move || drop(engine))
        }
        "track" => {
            let engine = TrackEngine::spawn(
                vec![TrackSpec {
                    id: TrackId::new(1),
                    name: "blocked".into(),
                    path: url.into(),
                    kind: TrackSpecKind::BigWig,
                }],
                4,
            );
            Box::new(move || drop(engine))
        }
        _ => {
            let engine = ReferenceEngine::spawn(4);
            engine.try_open(url.into()).unwrap();
            Box::new(move || drop(engine))
        }
    }
}

#[test]
fn dropping_an_engine_returns_while_its_source_io_is_still_blocked() {
    for (kind, file) in [
        ("scene", "blocked.bam"),
        ("track", "blocked.bw"),
        ("reference", "blocked.fa"),
    ] {
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}/{file}", server.server_addr());
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let server_worker = thread::spawn(move || {
            // Hold the first request until the test has observed the drop; then refuse
            // everything so the abandoned worker can finish.
            let request = server.recv().unwrap();
            entered_tx.send(()).unwrap();
            let _ = release_rx.recv();
            let _ = request.respond(Response::empty(StatusCode(404)));
        });
        let drop_engine = engine_blocked_on(kind, url);
        // The worker is now inside a read which cannot complete until `release_tx` fires.
        entered_rx
            .recv_timeout(Duration::from_secs(30))
            .unwrap_or_else(|_| panic!("the {kind} engine never started its source read"));

        let (dropped_tx, dropped_rx) = mpsc::channel();
        let drop_thread = thread::spawn(move || {
            drop_engine();
            dropped_tx.send(()).unwrap();
        });
        // A joining `Drop` can only return after the release below, so this receive is the
        // whole assertion; the timeout bounds the failing case and is never reached on success.
        let dropped = dropped_rx.recv_timeout(Duration::from_secs(30));
        release_tx.send(()).unwrap();
        dropped
            .unwrap_or_else(|_| panic!("dropping the {kind} engine waited for blocked source IO"));
        drop_thread.join().unwrap();
        server_worker.join().unwrap();
    }
}
