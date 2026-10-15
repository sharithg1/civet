//! Round-trip every wire message through postcard framing and through JSON.

use std::fmt::Debug;

use civet_proto::*;
use proptest::prelude::*;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Postcard frame round-trip + JSON round-trip.
fn rt<T>(msg: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let frame = encode_frame(msg).unwrap();
    let mut dec = FrameDecoder::new();
    dec.push(&frame);
    let back: T = dec.next().unwrap().expect("complete frame");
    assert_eq!(&back, msg, "postcard round-trip");
    dec.finish().unwrap();

    let json = serde_json::to_string(msg).unwrap();
    let back: T = serde_json::from_str(&json).unwrap();
    assert_eq!(&back, msg, "json round-trip: {json}");
}

fn pane_info() -> PaneInfo {
    PaneInfo {
        id: PaneId(7),
        worktree: Some(WorktreeId::from("/home/u/repo-worktrees/feat")),
        cwd: "/home/u/repo-worktrees/feat".into(),
        command: vec!["claude".into(), "--resume".into()],
        title: "claude".into(),
        size: TermSize {
            cols: 120,
            rows: 40,
            pixel_width: 1200,
            pixel_height: 800,
        },
        agent: Some("claude".into()),
        status: AgentStatus::NeedsInput,
        exit_code: None,
        exited: false,
        created_at_ms: 1_700_000_000_000,
        state: 99,
    }
}

fn repo() -> RepoInfo {
    RepoInfo {
        id: RepoId::from("repo-abc"),
        name: "orca".into(),
        path: "/home/u/orca".into(),
        default_branch: Some("main".into()),
    }
}

fn worktree() -> WorktreeInfo {
    WorktreeInfo {
        id: WorktreeId::from("/home/u/orca"),
        repo: RepoId::from("repo-abc"),
        path: "/home/u/orca".into(),
        branch: None,
        head: Some("0123456789abcdef0123456789abcdef01234567".into()),
        is_main: true,
        locked: false,
    }
}

fn screen_update(kind: UpdateKind) -> ScreenUpdate {
    ScreenUpdate {
        pane: PaneId(7),
        state: 12,
        base_state: if kind == UpdateKind::Full { 0 } else { 11 },
        kind,
        acked_input_seq: 33,
        size: TermSize::new(80, 24),
        history_lines: 1234,
        history_start: 1200,
        payload: b"\x1b[H\x1b[2J\x1b[1;31mhello\x1b[0m\x1b[5;3H".to_vec(),
    }
}

fn scrollback_page() -> ScrollbackPage {
    ScrollbackPage {
        req_id: 5,
        pane: PaneId(7),
        start: 100,
        lines: vec![
            ScrollbackLine {
                vt: b"\x1b[32mok\x1b[0m".to_vec(),
                wrapped: false,
            },
            ScrollbackLine {
                vt: "héllo wörld 🦀".as_bytes().to_vec(),
                wrapped: true,
            },
        ],
        oldest_available: 0,
        end_available: 500,
    }
}

fn all_requests() -> Vec<Request> {
    vec![
        Request::Ping { nonce: u64::MAX },
        Request::GetDaemonInfo,
        Request::ListRepos,
        Request::AddRepo {
            path: "/home/u/orca".into(),
        },
        Request::ListWorktrees {
            repo: RepoId::from("r"),
        },
        Request::CreateWorktree {
            repo: RepoId::from("r"),
            branch: "feat/x".into(),
            base: Some("main".into()),
            path: None,
        },
        Request::RemoveWorktree {
            worktree: WorktreeId::from("/w"),
            force: true,
        },
        Request::SpawnPane(SpawnPaneRequest {
            worktree: Some(WorktreeId::from("/w")),
            cwd: Some("/w/sub".into()),
            command: Some(CommandSpec::new("bash", ["-lc", "echo hi"])),
            env: vec![
                ("FOO".into(), "bar".into()),
                ("EMPTY".into(), String::new()),
            ],
            size: TermSize::new(100, 30),
            title: Some("agent".into()),
            agent: Some("codex".into()),
        }),
        Request::SpawnPane(SpawnPaneRequest {
            worktree: None,
            cwd: None,
            command: None,
            env: vec![],
            size: TermSize::default(),
            title: None,
            agent: None,
        }),
        Request::ResizePane {
            pane: PaneId(1),
            size: TermSize::new(1, 1),
        },
        Request::KillPane {
            pane: PaneId(1),
            signal: KillSignal::Kill,
        },
        Request::ListPanes,
        Request::SubscribePane {
            pane: PaneId(1),
            known_state: Some(42),
        },
        Request::SubscribePane {
            pane: PaneId(1),
            known_state: None,
        },
        Request::UnsubscribePane { pane: PaneId(1) },
        Request::FetchScrollback(ScrollbackRequest {
            req_id: 9,
            pane: PaneId(1),
            query: ScrollbackQuery::Before {
                end: None,
                limit: 200,
            },
        }),
        Request::SetAgentStatus {
            pane: PaneId(1),
            status: AgentStatus::Idle,
        },
        Request::LaunchAgent(LaunchAgentRequest {
            repo: RepoId::from("r"),
            branch: "agent/fix-bug".into(),
            base: Some("main".into()),
            worktree_path: None,
            command: CommandSpec::new("claude", ["--permission-mode", "plan"]),
            env: vec![("ANTHROPIC_LOG".into(), "debug".into())],
            size: TermSize::new(120, 40),
            title: None,
            agent: Some("claude".into()),
        }),
        Request::RemoveRepo {
            repo: RepoId::from("r"),
        },
    ]
}

fn all_responses() -> Vec<Result<Response, RpcError>> {
    vec![
        Ok(Response::Ok),
        Ok(Response::Pong {
            nonce: 1,
            server_time_ms: 2,
        }),
        Ok(Response::DaemonInfo(DaemonInfo {
            daemon_version: "0.1.0".into(),
            proto_version: PROTOCOL_VERSION,
            pid: 1,
            instance_id: 2,
            started_at_ms: 3,
            hostname: "box".into(),
        })),
        Ok(Response::Repos(vec![repo(), repo()])),
        Ok(Response::Repo(repo())),
        Ok(Response::Worktrees(vec![worktree()])),
        Ok(Response::Worktree(worktree())),
        Ok(Response::PaneSpawned(pane_info())),
        Ok(Response::Panes(vec![pane_info()])),
        Ok(Response::Panes(vec![])),
        Ok(Response::Subscribed(SubscribeInfo {
            pane: PaneId(7),
            state: 12,
            size: TermSize::new(80, 24),
            history_lines: 3,
            last_input_seq: 4,
        })),
        Ok(Response::Scrollback(scrollback_page())),
        Ok(Response::AgentLaunched {
            worktree: worktree(),
            pane: pane_info(),
        }),
        Err(RpcError::not_found(PaneId(3))),
        Err(RpcError::new(ErrorCode::Git, "fatal: bad ref")),
    ]
}

fn all_events() -> Vec<Event> {
    vec![
        Event::PaneCreated(pane_info()),
        Event::PaneExited {
            pane: PaneId(1),
            exit_code: Some(-1),
        },
        Event::PaneExited {
            pane: PaneId(1),
            exit_code: None,
        },
        Event::PaneClosed { pane: PaneId(1) },
        Event::PaneTitle {
            pane: PaneId(1),
            title: "vim".into(),
        },
        Event::PaneResized {
            pane: PaneId(1),
            size: TermSize::new(10, 10),
        },
        Event::AgentStatus(AgentStatusEvent {
            pane: PaneId(1),
            status: AgentStatus::Done,
            previous: AgentStatus::Running,
            source: AgentStatusSource::EscapeCode,
            message: Some("Task complete".into()),
            at_ms: 5,
        }),
        Event::Bell { pane: PaneId(1) },
        Event::ReposChanged,
        Event::WorktreesChanged {
            repo: RepoId::from("r"),
        },
    ]
}

#[test]
fn hello_and_bootstrap() {
    rt(&ClientHello::new(
        SessionKey::generate().unwrap(),
        ClientId::generate().unwrap(),
        "civet-desktop",
        "0.1.0",
    ));
    rt(&ClientHello::new(
        SessionKey::generate().unwrap(),
        ClientId::generate().unwrap(),
        "civet-cli",
        "0.1.0",
    )
    .with_resume(Some(ResumeToken::generate().unwrap())));
    rt(&HelloReply::Accepted(ServerHello {
        proto_version: PROTOCOL_VERSION,
        daemon_version: "0.1.0".into(),
        instance_id: 1,
        resumed: false,
        server_time_ms: now_ms(),
        resume_token: ResumeToken::generate().unwrap(),
        resumed_subscriptions: vec![],
    }));
    rt(&HelloReply::Accepted(ServerHello {
        proto_version: PROTOCOL_VERSION,
        daemon_version: "0.1.0".into(),
        instance_id: 1,
        resumed: true,
        server_time_ms: now_ms(),
        resume_token: ResumeToken([9; 32]),
        resumed_subscriptions: vec![
            SubscribeInfo {
                pane: PaneId(1),
                state: 10,
                size: TermSize::new(80, 24),
                history_lines: 0,
                last_input_seq: 7,
            },
            SubscribeInfo {
                pane: PaneId(2),
                state: 1,
                size: TermSize::new(200, 60),
                history_lines: 99_999,
                last_input_seq: 0,
            },
        ],
    }));
    for reason in [
        RejectReason::BadKey,
        RejectReason::VersionMismatch,
        RejectReason::BadMagic,
        RejectReason::ShuttingDown,
    ] {
        rt(&HelloReply::Rejected(HelloReject::new(reason, "x")));
    }
    rt(&BootstrapInfo::new(
        "0.1.0",
        60001,
        SessionKey::generate().unwrap(),
        CertFingerprint([0x5a; 32]),
    ));
}

#[test]
fn stream_headers() {
    for s in [
        StreamOpen::Control,
        StreamOpen::Screen { pane: PaneId(3) },
        StreamOpen::Scrollback,
    ] {
        rt(&s);
    }
}

#[test]
fn control_requests_and_client_messages() {
    for (i, request) in all_requests().into_iter().enumerate() {
        let req = ControlRequest {
            id: i as u64,
            request,
        };
        rt(&req);
        rt(&ClientMessage::Request(req));
    }
    rt(&ClientMessage::Input(InputMessage {
        pane: PaneId(1),
        seq: 1,
        data: b"ls -la\r\x1b[A\x03".to_vec(),
    }));
    rt(&ClientMessage::Input(InputMessage {
        pane: PaneId(1),
        seq: u64::MAX,
        data: vec![],
    }));
    rt(&ClientMessage::Ack(ScreenAck {
        pane: PaneId(1),
        state: 77,
    }));
    rt(&ClientMessage::Goodbye);
}

#[test]
fn control_responses_events_and_server_messages() {
    for (i, result) in all_responses().into_iter().enumerate() {
        let resp = ControlResponse {
            id: i as u64,
            result,
        };
        rt(&resp);
        rt(&ServerMessage::Response(resp));
    }
    for ev in all_events() {
        rt(&ev);
        rt(&ServerMessage::Event(ev));
    }
    rt(&ServerMessage::Screen(screen_update(UpdateKind::Full)));
    rt(&ServerMessage::Screen(screen_update(UpdateKind::Diff)));
    rt(&ServerMessage::InputAck {
        pane: PaneId(2),
        seq: 9,
    });
    rt(&ServerMessage::Goodbye {
        reason: "upgrade".into(),
    });
}

#[test]
fn scrollback_stream_messages() {
    for query in [
        ScrollbackQuery::Before {
            end: Some(500),
            limit: 100,
        },
        ScrollbackQuery::Before {
            end: None,
            limit: 0,
        },
        ScrollbackQuery::Range { start: 0, end: 10 },
    ] {
        rt(&ScrollbackRequest {
            req_id: 1,
            pane: PaneId(1),
            query,
        });
    }
    rt(&Ok::<ScrollbackPage, RpcError>(scrollback_page()));
    rt(&Err::<ScrollbackPage, RpcError>(RpcError::not_found(
        "pane",
    )));
}

#[test]
fn json_shapes_for_ui() {
    // The desktop UI consumes these as JSON; pin a few shapes.
    let v = serde_json::to_value(pane_info()).unwrap();
    assert_eq!(v["id"], 7);
    assert_eq!(v["status"], "needsInput");
    assert_eq!(v["createdAtMs"], 1_700_000_000_000u64);
    assert_eq!(v["size"]["cols"], 120);

    let v = serde_json::to_value(Event::AgentStatus(AgentStatusEvent {
        pane: PaneId(1),
        status: AgentStatus::Done,
        previous: AgentStatus::Running,
        source: AgentStatusSource::Hook,
        message: None,
        at_ms: 5,
    }))
    .unwrap();
    assert_eq!(v["agentStatus"]["status"], "done");
    assert_eq!(v["agentStatus"]["source"], "hook");

    let v = serde_json::to_value(Request::ListPanes).unwrap();
    assert_eq!(v, "listPanes");

    let v = serde_json::to_value(Response::AgentLaunched {
        worktree: worktree(),
        pane: pane_info(),
    })
    .unwrap();
    assert_eq!(v["agentLaunched"]["pane"]["id"], 7);
    assert_eq!(v["agentLaunched"]["worktree"]["isMain"], true);

    let v = serde_json::to_value(screen_update(UpdateKind::Full)).unwrap();
    assert_eq!(v["kind"], "full");
    assert_eq!(v["ackedInputSeq"], 33);
}

#[test]
fn concatenated_mixed_frames_decode_in_order() {
    let mut stream = encode_frame(&StreamOpen::Control).unwrap();
    let hello = ClientHello::new(SessionKey([1; 32]), ClientId([2; 16]), "cli", "0");
    stream.extend(encode_frame(&hello).unwrap());
    let msgs: Vec<ClientMessage> = all_requests()
        .into_iter()
        .enumerate()
        .map(|(i, request)| {
            ClientMessage::Request(ControlRequest {
                id: i as u64,
                request,
            })
        })
        .collect();
    for m in &msgs {
        stream.extend(encode_frame(m).unwrap());
    }

    let mut dec = FrameDecoder::new();
    // Feed in awkward chunk sizes.
    for chunk in stream.chunks(7) {
        dec.push(chunk);
    }
    assert_eq!(dec.next::<StreamOpen>().unwrap(), Some(StreamOpen::Control));
    let raw = dec.next_raw().unwrap().unwrap();
    assert_eq!(hello::peek_hello_version(&raw).unwrap(), PROTOCOL_VERSION);
    assert_eq!(decode::<ClientHello>(&raw).unwrap(), hello);
    for m in &msgs {
        assert_eq!(dec.next::<ClientMessage>().unwrap().as_ref(), Some(m));
    }
    assert!(dec.next_raw().unwrap().is_none());
    dec.finish().unwrap();
}

#[test]
fn garbage_never_panics() {
    // Truncations of a valid message must error, not panic.
    let full = encode(&ServerMessage::Response(ControlResponse {
        id: 1,
        result: Ok(Response::PaneSpawned(pane_info())),
    }))
    .unwrap();
    for cut in 0..full.len() {
        assert!(decode::<ServerMessage>(&full[..cut]).is_err());
    }
}

proptest! {
    #[test]
    fn prop_input_roundtrip(pane in any::<u64>(), seq in any::<u64>(), data in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let m = ClientMessage::Input(InputMessage { pane: PaneId(pane), seq, data });
        let f = encode_frame(&m).unwrap();
        let mut dec = FrameDecoder::new();
        dec.push(&f);
        prop_assert_eq!(dec.next::<ClientMessage>().unwrap(), Some(m));
    }

    #[test]
    fn prop_screen_update_roundtrip(
        state in any::<u64>(), base in any::<u64>(), seq in any::<u64>(),
        cols in any::<u16>(), rows in any::<u16>(), hist in any::<u64>(), full in any::<bool>(),
        payload in proptest::collection::vec(any::<u8>(), 0..8192),
    ) {
        let m = ServerMessage::Screen(ScreenUpdate {
            pane: PaneId(1), state, base_state: base,
            kind: if full { UpdateKind::Full } else { UpdateKind::Diff },
            acked_input_seq: seq, size: TermSize::new(cols, rows), history_lines: hist, history_start: hist / 2, payload,
        });
        prop_assert_eq!(decode::<ServerMessage>(&encode(&m).unwrap()).unwrap(), m);
    }

    #[test]
    fn prop_random_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
        let _ = decode::<ServerMessage>(&bytes);
        let _ = decode::<ClientMessage>(&bytes);
        let _ = decode::<ClientHello>(&bytes);
        let _ = hello::peek_hello_version(&bytes);
        let mut dec = FrameDecoder::with_max_len(1024);
        dec.push(&bytes);
        while let Ok(Some(_)) = dec.next_raw() {}
    }
}
