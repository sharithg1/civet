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
        Request::UploadFile {
            name: "shot.png".into(),
            data: vec![0x89, b'P', b'N', b'G'],
        },
        Request::ListPullRequests {
            repo: RepoId::from("r"),
        },
        Request::ListFiles {
            worktree: WorktreeId::from("/w"),
            dir: "src/bin".into(),
            include_ignored: true,
        },
        Request::ReadFile {
            worktree: WorktreeId::from("/w"),
            path: "src/main.rs".into(),
            max_bytes: 65_536,
        },
        Request::ListAllFiles {
            worktree: WorktreeId::from("/w"),
        },
        Request::GitStatus {
            worktree: WorktreeId::from("/w"),
        },
        Request::GitDiff {
            worktree: WorktreeId::from("/w"),
            path: "src/new.rs".into(),
            old_path: Some("src/old.rs".into()),
            staged: true,
            context_lines: 3,
        },
        Request::FileOp {
            worktree: WorktreeId::from("/w"),
            op: FileOp::CreateFile {
                path: "a.txt".into(),
            },
        },
        Request::FileOp {
            worktree: WorktreeId::from("/w"),
            op: FileOp::CreateDir {
                path: "src/new".into(),
            },
        },
        Request::FileOp {
            worktree: WorktreeId::from("/w"),
            op: FileOp::Move {
                from: "a".into(),
                to: "b/a".into(),
            },
        },
        Request::FileOp {
            worktree: WorktreeId::from("/w"),
            op: FileOp::Copy {
                from: "a".into(),
                to: "a copy".into(),
            },
        },
        Request::FileOp {
            worktree: WorktreeId::from("/w"),
            op: FileOp::Trash { path: "a".into() },
        },
        Request::FileOp {
            worktree: WorktreeId::from("/w"),
            op: FileOp::Restore {
                trash_id: "1-2-3".into(),
            },
        },
        Request::FileOp {
            worktree: WorktreeId::from("/w"),
            op: FileOp::Write {
                path: "up.png".into(),
                data: vec![1, 2, 3],
            },
        },
        Request::ReadFileChunk {
            worktree: WorktreeId::from("/w"),
            path: "big.bin".into(),
            offset: 8 << 20,
            len: 8 << 20,
        },
        Request::GitBranch {
            worktree: WorktreeId::from("/w"),
        },
        Request::ListProjects,
        Request::AddProject {
            path: "~/notes".into(),
        },
        Request::ListDirs {
            path: "~/src".into(),
        },
        Request::HostStats {
            since: None,
            processes: false,
        },
        Request::HostStats {
            since: Some(41),
            processes: true,
        },
        Request::WorktreeLifecycle {
            repo: RepoId("abcd".into()),
            fetch: true,
            review: false,
        },
        Request::UpdateWorktree {
            worktree: WorktreeId::from("/w"),
            strategy: UpdateStrategy::Rebase,
        },
        Request::UpdateWorktree {
            worktree: WorktreeId::from("/w"),
            strategy: UpdateStrategy::Merge,
        },
        Request::SendPrompt {
            pane: PaneId(3),
            text: "run the tests\nthen commit".into(),
        },
        Request::QueuePrompt {
            pane: PaneId(3),
            text: "now open a PR".into(),
            trigger: PromptTrigger::Done,
        },
        Request::EditQueuedPrompt {
            id: 9,
            text: "yes".into(),
            trigger: PromptTrigger::DoneOrNeedsInput,
        },
        Request::CancelQueuedPrompt { id: 9 },
        Request::ListQueuedPrompts,
    ]
    .into_iter()
    .chain(git_actions().into_iter().map(|action| Request::GitAction {
        worktree: WorktreeId::from("/w"),
        action,
    }))
    .collect()
}

fn discard_snapshot() -> GitDiscardSnapshot {
    GitDiscardSnapshot {
        entries: vec![
            GitDiscardEntry {
                path: "a.rs".into(),
                file: Some(GitBlobRef {
                    oid: "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391".into(),
                    mode: 0o100755,
                }),
                restore_index: true,
                index: None,
                untracked: false,
            },
            GitDiscardEntry {
                path: "new.txt".into(),
                file: None,
                restore_index: false,
                index: None,
                untracked: true,
            },
        ],
    }
}

fn git_actions() -> Vec<GitAction> {
    vec![
        GitAction::Stage {
            paths: vec!["a.rs".into(), "dir/b c.rs".into()],
        },
        GitAction::Unstage { paths: vec![] },
        GitAction::Discard {
            paths: vec!["a.rs".into()],
            staged: true,
        },
        GitAction::RestoreDiscarded(discard_snapshot()),
        GitAction::Commit {
            message: "Fix it\n\nDetails ✓".into(),
            amend: true,
            all: false,
        },
        GitAction::AcceptConflict {
            paths: vec!["a.rs".into()],
            side: ConflictSide::Theirs,
        },
        GitAction::Push,
        GitAction::Pull,
        GitAction::Fetch,
        GitAction::UndoCommit {
            head: "abc123".into(),
        },
        GitAction::StashAll,
        GitAction::StashPop,
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
        Ok(Response::FileUploaded {
            path: "/tmp/civet-drops/1/shot.png".into(),
        }),
        Ok(Response::PullRequests(vec![PullRequestInfo {
            number: 42,
            branch: "feat/x".into(),
            title: "Add x".into(),
            url: "https://github.com/o/r/pull/42".into(),
            state: PullRequestState::Merged,
        }])),
        Ok(Response::Files(DirListing {
            entries: vec![
                FileEntry {
                    name: "src".into(),
                    kind: FileKind::Dir,
                    size: 0,
                    ignored: false,
                },
                FileEntry {
                    name: "link".into(),
                    kind: FileKind::Symlink,
                    size: 7,
                    ignored: true,
                },
            ],
            truncated: false,
        })),
        Ok(Response::FilePreview(FilePreview {
            size: 1_000_000,
            binary: false,
            truncated: true,
            text: "fn main() {}\n".into(),
        })),
        Ok(Response::AllFiles(FileList {
            paths: vec!["README.md".into(), "src/main.rs".into()],
            truncated: false,
        })),
        Ok(Response::GitStatus(GitStatusInfo {
            staged: vec![GitFileChange {
                path: "b.rs".into(),
                old_path: Some("a.rs".into()),
                kind: GitChangeKind::Renamed,
                additions: Some(2),
                deletions: Some(1),
            }],
            unstaged: vec![GitFileChange {
                path: "img.png".into(),
                old_path: None,
                kind: GitChangeKind::Modified,
                additions: None,
                deletions: None,
            }],
            untracked: vec![GitFileChange {
                path: "new.txt".into(),
                old_path: None,
                kind: GitChangeKind::Untracked,
                additions: Some(3),
                deletions: Some(0),
            }],
            conflicted: vec![],
            truncated: false,
        })),
        Ok(Response::GitDiff(GitDiffInfo {
            path: "b.rs".into(),
            binary: false,
            too_large: false,
            additions: Some(1),
            deletions: Some(0),
            diff: "diff --git a/b.rs b/b.rs\n@@ -1 +1,2 @@\n x\n+y\n".into(),
        })),
        Ok(Response::FileOpDone(FileOpResult {
            path: "a".into(),
            trash_id: Some("1-2-3".into()),
        })),
        Ok(Response::FileOpDone(FileOpResult {
            path: "b".into(),
            trash_id: None,
        })),
        Ok(Response::FileChunk(FileChunk {
            size: 10,
            data: vec![0, 1, 2],
        })),
        Ok(Response::Projects(vec![
            ProjectInfo {
                repo: repo(),
                kind: ProjectKind::Git,
            },
            ProjectInfo {
                repo: RepoInfo {
                    default_branch: None,
                    ..repo()
                },
                kind: ProjectKind::Folder,
            },
        ])),
        Ok(Response::Project(ProjectInfo {
            repo: repo(),
            kind: ProjectKind::Folder,
        })),
        Ok(Response::GitBranch(GitBranchInfo {
            branch: Some("feat/x".into()),
            head: Some("abc123".into()),
            upstream: Some("origin/feat/x".into()),
            ahead: 2,
            behind: 1,
            can_undo_commit: true,
            stashes: 1,
            has_remote: true,
        })),
        Ok(Response::GitBranch(GitBranchInfo {
            branch: None,
            head: None,
            upstream: None,
            ahead: 0,
            behind: 0,
            can_undo_commit: false,
            stashes: 0,
            has_remote: false,
        })),
        Ok(Response::GitDiscarded(discard_snapshot())),
        Ok(Response::Dirs(HostDirListing {
            path: "/home/u/src".into(),
            dirs: vec![
                HostDir {
                    name: "civet".into(),
                    git_repo: true,
                },
                HostDir {
                    name: "notes".into(),
                    git_repo: false,
                },
            ],
            truncated: false,
        })),
        Ok(Response::HostStats(host_stats())),
        Ok(Response::WorktreeLifecycle(repo_lifecycle())),
        Ok(Response::WorktreeUpdated(WorktreeUpdate {
            base: "origin/main".into(),
            outcome: UpdateOutcome::RebaseConflicts,
            conflicts: vec!["src/a.rs".into()],
        })),
        Ok(Response::WorktreeUpdated(WorktreeUpdate {
            base: "main".into(),
            outcome: UpdateOutcome::UpToDate,
            conflicts: vec![],
        })),
        Ok(Response::QueuedPrompt(queued_prompt())),
        Ok(Response::QueuedPrompts(vec![
            queued_prompt(),
            queued_prompt(),
        ])),
        Ok(Response::QueuedPrompts(vec![])),
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
        Event::Clipboard {
            pane: PaneId(1),
            text: "copied ✓".into(),
        },
        Event::PromptQueue {
            pane: PaneId(7),
            prompts: vec![queued_prompt()],
            sent: None,
        },
        Event::PromptQueue {
            pane: PaneId(7),
            prompts: vec![],
            sent: Some(4),
        },
    ]
}

fn queued_prompt() -> QueuedPrompt {
    QueuedPrompt {
        id: 4,
        pane: PaneId(7),
        text: "now run the e2e tests".into(),
        trigger: PromptTrigger::DoneOrNeedsInput,
        created_at_ms: 1_700_000_000_000,
    }
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

#[test]
fn clipboard_event_is_appended() {
    // Appended within protocol 1: existing variants keep their indices and
    // Clipboard takes the next one (postcard varint discriminant).
    assert_eq!(encode(&Event::ReposChanged).unwrap(), vec![7]);
    let bytes = encode(&Event::Clipboard {
        pane: PaneId(3),
        text: "hi".into(),
    })
    .unwrap();
    assert_eq!(bytes, vec![9, 3, 2, b'h', b'i']);
    let v = serde_json::to_value(Event::Clipboard {
        pane: PaneId(3),
        text: "hi".into(),
    })
    .unwrap();
    assert_eq!(
        v,
        serde_json::json!({"clipboard": {"pane": 3, "text": "hi"}})
    );
}

#[test]
fn project_requests_are_appended() {
    // Appended within protocol 1 after ReadFileChunk (27) / FileChunk (22).
    assert_eq!(encode(&Request::ListProjects).unwrap(), vec![28]);
    assert_eq!(
        encode(&Request::AddProject { path: "a".into() }).unwrap(),
        vec![29, 1, b'a']
    );
    assert_eq!(encode(&Response::Projects(vec![])).unwrap(), vec![23, 0]);
    assert_eq!(encode(&ProjectKind::Folder).unwrap(), vec![1]);
    let v = serde_json::to_value(Response::Project(ProjectInfo {
        repo: repo(),
        kind: ProjectKind::Folder,
    }))
    .unwrap();
    assert_eq!(v["project"]["kind"], "folder");
    assert_eq!(v["project"]["repo"]["defaultBranch"], "main");
}

fn host_stats() -> HostStats {
    HostStats {
        interval_ms: 2000,
        samples: vec![StatsSample {
            seq: 42,
            at_ms: 1_700_000_000_000,
            cpu: 625,
            mem_used: 41 << 30,
            swap_used: 0,
            net_rx: 12_000_000,
            net_tx: 2_000_000,
            disk_read: 120_000_000,
            disk_write: 40_000_000,
            gpus: vec![880],
        }],
        cores: vec![1000, 250],
        mem_total: 64 << 30,
        swap_total: 8 << 30,
        load: [820, 610, 400],
        uptime_secs: 86_400,
        disks: vec![DiskSpace {
            mount: "/".into(),
            total: 1 << 40,
            available: 300 << 30,
        }],
        gpus: vec![GpuInfo {
            name: "NVIDIA A10G".into(),
            mem_used: 20 << 30,
            mem_total: 24 << 30,
        }],
        processes: vec![ProcessUsage {
            pid: 4242,
            name: "cargo".into(),
            command: "cargo test".into(),
            cpu: 3400,
            mem: 3 << 30,
            pane: Some(PaneId(7)),
            worktree: Some(WorktreeId::from("/w")),
        }],
    }
}

fn repo_lifecycle() -> RepoLifecycle {
    let pr = PullRequestInfo {
        number: 9,
        branch: "feat/x".into(),
        title: "X".into(),
        url: "https://github.com/o/r/pull/9".into(),
        state: PullRequestState::Merged,
    };
    RepoLifecycle {
        base: Some("origin/main".into()),
        worktrees: vec![
            WorktreeLifecycle {
                worktree: WorktreeId::from("/r"),
                branch: Some("main".into()),
                ahead: 0,
                behind: 3,
                upstream: Some("origin/main".into()),
                upstream_ahead: 0,
                upstream_behind: 3,
                upstream_gone: false,
                unpushed: 0,
                dirty: None,
                pull_request: None,
                stale: vec![],
            },
            WorktreeLifecycle {
                worktree: WorktreeId::from("/w"),
                branch: Some("feat/x".into()),
                ahead: 2,
                behind: 1,
                upstream: Some("origin/feat/x".into()),
                upstream_ahead: 0,
                upstream_behind: 0,
                upstream_gone: true,
                unpushed: 2,
                dirty: Some(4),
                pull_request: Some(pr),
                stale: vec![StaleReason::UpstreamGone, StaleReason::PullRequestMerged],
            },
        ],
        fetch_error: Some("could not resolve host".into()),
        pull_requests_error: None,
    }
}

#[test]
fn worktree_lifecycle_is_appended() {
    // Appended within protocol 1 after HostStats (31) / HostStats (26).
    let req = encode(&Request::WorktreeLifecycle {
        repo: RepoId("r".into()),
        fetch: false,
        review: true,
    })
    .unwrap();
    assert_eq!(req, vec![32, 1, b'r', 0, 1]);
    let req = encode(&Request::UpdateWorktree {
        worktree: WorktreeId::from("/"),
        strategy: UpdateStrategy::Merge,
    })
    .unwrap();
    assert_eq!(req, vec![33, 1, b'/', 1]);
    assert_eq!(
        encode(&Response::WorktreeLifecycle(repo_lifecycle())).unwrap()[0],
        27
    );
    let v = serde_json::to_value(Response::WorktreeLifecycle(repo_lifecycle())).unwrap();
    let w = &v["worktreeLifecycle"]["worktrees"][1];
    assert_eq!(w["upstreamGone"], true);
    assert_eq!(
        w["stale"],
        serde_json::json!(["upstreamGone", "pullRequestMerged"])
    );
    assert_eq!(w["pullRequest"]["state"], "merged");
    let v = serde_json::to_value(Response::WorktreeUpdated(WorktreeUpdate {
        base: "origin/main".into(),
        outcome: UpdateOutcome::MergeConflicts,
        conflicts: vec!["a".into()],
    }))
    .unwrap();
    assert_eq!(v["worktreeUpdated"]["outcome"], "mergeConflicts");
}

#[test]
fn host_stats_is_appended() {
    // Appended within protocol 1 after ListDirs (30) / Dirs (25).
    let req = encode(&Request::HostStats {
        since: None,
        processes: true,
    })
    .unwrap();
    assert_eq!(req, vec![31, 0, 1]);
    let resp = encode(&Response::HostStats(host_stats())).unwrap();
    assert_eq!(resp[0], 26);
    let v = serde_json::to_value(Response::HostStats(host_stats())).unwrap();
    assert_eq!(v["hostStats"]["samples"][0]["memUsed"], 41u64 << 30);
    assert_eq!(v["hostStats"]["load"], serde_json::json!([820, 610, 400]));
    assert_eq!(v["hostStats"]["processes"][0]["worktree"], "/w");
}

#[test]
fn prompts_are_appended() {
    // Appended within protocol 1 after UpdateWorktree (request 33) /
    // WorktreeUpdated (response 28) and Clipboard (event 9).
    let req = encode(&Request::SendPrompt {
        pane: PaneId(2),
        text: "hi".into(),
    })
    .unwrap();
    assert_eq!(req, vec![34, 2, 2, b'h', b'i']);
    assert_eq!(encode(&Request::ListQueuedPrompts).unwrap(), vec![38]);
    let resp = encode(&Response::QueuedPrompts(vec![])).unwrap();
    assert_eq!(resp, vec![30, 0]);
    let ev = encode(&Event::PromptQueue {
        pane: PaneId(2),
        prompts: vec![],
        sent: None,
    })
    .unwrap();
    assert_eq!(ev, vec![10, 2, 0, 0]);
    assert_eq!(
        Event::PromptQueue {
            pane: PaneId(2),
            prompts: vec![],
            sent: None
        }
        .pane(),
        Some(PaneId(2))
    );
    let v = serde_json::to_value(Event::PromptQueue {
        pane: PaneId(7),
        prompts: vec![queued_prompt()],
        sent: Some(4),
    })
    .unwrap();
    assert_eq!(
        v["promptQueue"]["prompts"][0]["trigger"],
        "doneOrNeedsInput"
    );
    assert_eq!(v["promptQueue"]["sent"], 4);
}

#[test]
fn list_dirs_is_appended() {
    // Appended within protocol 1 after AddProject (29) / Project (24).
    let req = encode(&Request::ListDirs { path: "~".into() }).unwrap();
    assert_eq!(req, vec![30, 1, b'~']);
    let resp = encode(&Response::Dirs(HostDirListing {
        path: "/".into(),
        dirs: vec![],
        truncated: false,
    }))
    .unwrap();
    assert_eq!(resp, vec![25, 1, b'/', 0, 0]);
    let v = serde_json::to_value(Response::Dirs(HostDirListing {
        path: "/h".into(),
        dirs: vec![HostDir {
            name: "r".into(),
            git_repo: true,
        }],
        truncated: false,
    }))
    .unwrap();
    assert_eq!(
        v,
        serde_json::json!({"dirs": {"path": "/h", "dirs": [{"name": "r", "gitRepo": true}], "truncated": false}})
    );
}

/// Struct-variant fields must be camelCase in JSON too (`rename_all` on an
/// enum only renames variants; the UI reads `exitCode`, not `exit_code`).
#[test]
fn json_struct_variant_fields_are_camel_case() {
    let v = serde_json::to_value(Event::PaneExited {
        pane: PaneId(3),
        exit_code: Some(1),
    })
    .unwrap();
    assert_eq!(
        v,
        serde_json::json!({"paneExited": {"pane": 3, "exitCode": 1}})
    );

    let v = serde_json::to_value(Request::SubscribePane {
        pane: PaneId(2),
        known_state: Some(9),
    })
    .unwrap();
    assert_eq!(
        v,
        serde_json::json!({"subscribePane": {"pane": 2, "knownState": 9}})
    );

    let v = serde_json::to_value(Response::Pong {
        nonce: 1,
        server_time_ms: 2,
    })
    .unwrap();
    assert_eq!(
        v,
        serde_json::json!({"pong": {"nonce": 1, "serverTimeMs": 2}})
    );

    // No snake_case key anywhere in any event / request / response.
    fn no_snake(v: &serde_json::Value, what: &str) {
        match v {
            serde_json::Value::Object(m) => {
                for (k, v) in m {
                    assert!(!k.contains('_'), "snake_case key {k:?} in {what}");
                    no_snake(v, what);
                }
            }
            serde_json::Value::Array(a) => a.iter().for_each(|v| no_snake(v, what)),
            _ => {}
        }
    }
    for e in all_events() {
        no_snake(&serde_json::to_value(&e).unwrap(), &format!("{e:?}"));
        rt(&e);
    }
    for r in all_requests() {
        no_snake(&serde_json::to_value(&r).unwrap(), &format!("{r:?}"));
        rt(&r);
    }
    for r in all_responses() {
        no_snake(&serde_json::to_value(&r).unwrap(), &format!("{r:?}"));
    }
}
