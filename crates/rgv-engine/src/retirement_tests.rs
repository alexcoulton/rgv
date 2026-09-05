use crate::{ReferenceEngine, TrackEngine, TrackSpec, TrackSpecKind};
use rgv_core::TrackId;
use std::{sync::mpsc, thread, time::Duration};
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
