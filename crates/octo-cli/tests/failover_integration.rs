use octo_core::{
    Blackboard, MeshConfig, MeshError, MeshRouter, PeerConfig, PeerHealth, RouteRequest,
};
use std::collections::HashMap;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    // crates/octo-cli -> workspace root
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn test_config() -> MeshConfig {
    let mut peers = HashMap::new();
    peers.insert(
        "mock_primary".to_string(),
        PeerConfig {
            adapter: "mock".to_string(),
            command: None,
            args: vec![],
            timeout_secs: 5,
            cooldown_secs: Some(120),
            env: HashMap::new(),
        },
    );
    peers.insert(
        "mock_backup".to_string(),
        PeerConfig {
            adapter: "mock".to_string(),
            command: None,
            args: vec![],
            timeout_secs: 5,
            cooldown_secs: Some(120),
            env: HashMap::new(),
        },
    );

    let mut roles = HashMap::new();
    roles.insert(
        "reviewer".to_string(),
        vec!["mock_primary".to_string(), "mock_backup".to_string()],
    );

    // Use python3 directly with PYTHONPATH=adapters so integration tests work even without uv sync
    let mut env_override = HashMap::new();
    env_override.insert("PYTHONPATH".to_string(), "adapters".to_string());
    for peer in peers.values_mut() {
        peer.env = env_override.clone();
    }

    MeshConfig {
        max_depth: 2,
        default_cooldown_secs: 120,
        adapter_runner: vec![
            "python3".to_string(),
            "-m".to_string(),
            "octomesh_adapters.runner".to_string(),
        ],
        roles,
        peers,
    }
}

#[tokio::test]
async fn test_automatic_failover_on_rate_limit_and_cooldown_persistence() {
    let config = test_config();
    let board = Blackboard::open_in_memory().expect("in-memory sqlite");
    let router = MeshRouter::new(config, board.clone(), repo_root());

    // 1. Prompt triggers [SIMULATE_RATE_LIMIT] on mock_primary; router must fail over to mock_backup
    let result = router
        .route(RouteRequest {
            target: "reviewer".to_string(),
            prompt: "[SIMULATE_RATE_LIMIT] Audit SQL queries for injection risks".to_string(),
            caller: "claude_cli".to_string(),
            depth: 1,
            include_diff: false,
            readonly: true,
        })
        .await
        .expect("failover to mock_backup should succeed");

    assert_eq!(result.resolved_peer, "mock_backup");
    assert_eq!(result.attempts.len(), 2);
    assert_eq!(result.attempts[0].peer, "mock_primary");
    assert_eq!(result.attempts[1].peer, "mock_backup");
    assert!(result.output.contains("mock_backup"));

    // 2. Verify SQLite blackboard recorded mock_primary as rate_limited on cooldown
    let states = board.list_peer_states().unwrap();
    let primary_state = states
        .iter()
        .find(|s| s.peer_name == "mock_primary")
        .expect("mock_primary state present");
    assert_eq!(primary_state.health, PeerHealth::RateLimited);
    assert!(primary_state.cooldown_until.is_some());

    // 3. Subsequent request to 'reviewer' must skip mock_primary immediately due to active cooldown
    let second = router
        .route(RouteRequest {
            target: "reviewer".to_string(),
            prompt: "Second review request".to_string(),
            caller: "claude_cli".to_string(),
            depth: 1,
            include_diff: false,
            readonly: true,
        })
        .await
        .expect("second request should route directly to mock_backup");

    assert_eq!(second.resolved_peer, "mock_backup");
    assert!(second.attempts[0].skipped_cooldown);
}

#[tokio::test]
async fn test_anti_loop_guard_rejects_depth_and_self_invocation() {
    let config = test_config();
    let board = Blackboard::open_in_memory().unwrap();
    let router = MeshRouter::new(config, board, repo_root());

    // Exceed max_depth (2)
    let depth_err = router
        .route(RouteRequest {
            target: "reviewer".to_string(),
            prompt: "Looping call".to_string(),
            caller: "claude_cli".to_string(),
            depth: 3,
            include_diff: false,
            readonly: true,
        })
        .await
        .expect_err("depth 3 must be rejected");

    assert!(matches!(
        depth_err,
        MeshError::MaxDepthExceeded {
            depth: 3,
            max_depth: 2
        }
    ));

    // Reject caller == target
    let self_err = router
        .route(RouteRequest {
            target: "mock_primary".to_string(),
            prompt: "Calling myself".to_string(),
            caller: "mock_primary".to_string(),
            depth: 1,
            include_diff: false,
            readonly: false,
        })
        .await
        .expect_err("self invocation must be rejected");

    assert!(matches!(self_err, MeshError::SelfInvocation { .. }));

    // Reject ancestor cycle and inherited min_depth
    let nested_router = MeshRouter::new(
        test_config(),
        Blackboard::open_in_memory().unwrap(),
        repo_root(),
    )
    .with_env_context(3, vec!["claude".to_string()]);
    let inherited_err = nested_router
        .route(RouteRequest {
            target: "reviewer".to_string(),
            prompt: "Nested call forgetting to increment depth".to_string(),
            caller: "vibe".to_string(),
            depth: 1,
            include_diff: false,
            readonly: true,
        })
        .await
        .expect_err("inherited depth > max_depth must be rejected");
    assert!(matches!(
        inherited_err,
        MeshError::MaxDepthExceeded {
            depth: 3,
            max_depth: 2
        }
    ));
}

#[tokio::test]
async fn test_reset_peer_health_clears_cooldown() {
    let config = test_config();
    let board = Blackboard::open_in_memory().unwrap();
    let router = MeshRouter::new(config, board.clone(), repo_root());

    let _ = router
        .route(RouteRequest {
            target: "reviewer".to_string(),
            prompt: "[SIMULATE_RATE_LIMIT] trigger cooldown".to_string(),
            caller: "human-cli".to_string(),
            depth: 0,
            include_diff: false,
            readonly: true,
        })
        .await
        .expect("failover succeeds");

    assert!(!board.is_peer_available("mock_primary").unwrap());
    board.reset_peer_health(Some("mock_primary")).unwrap();
    assert!(board.is_peer_available("mock_primary").unwrap());
}
