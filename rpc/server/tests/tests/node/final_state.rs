use test_helpers::{System, assert_timely2, send_block, setup_rpc_client_and_server};

/// Genesis alone hashes the same on every node; an unconfirmed block leaves the
/// hash unchanged but pending
#[test]
fn final_state() {
    let mut system = System::new();
    let node = system.make_node();
    let server = setup_rpc_client_and_server(node.clone(), false);

    let genesis_only = node
        .runtime
        .block_on(async { server.client.final_state().await.unwrap() });
    assert_eq!(genesis_only.accounts, 1.into());
    assert_eq!(genesis_only.pending, 0.into());
    assert_eq!(genesis_only.all_settled, true.into());

    send_block(node.clone());
    assert_timely2(|| node.aec.len() == 1);

    let with_election = node
        .runtime
        .block_on(async { server.client.final_state().await.unwrap() });
    assert_eq!(with_election.hash, genesis_only.hash);
    assert_eq!(with_election.pending, 1.into());
    assert_eq!(with_election.all_settled, false.into());
    assert!(with_election.conflicting.is_empty());
}
