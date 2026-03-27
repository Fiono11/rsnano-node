use rsnano_node::consensus::election::ElectionBehavior;
use rsnano_types::DEV_GENESIS_KEY;
use test_helpers::{assert_timely2, setup_chains, System};

/*
 * Ensure account gets activated for a single unconfirmed account chain
 */
#[test]
pub fn activate_one() {
    let mut system = System::new();
    let node = system.make_node();

    // Needs to be greater than optimistic scheduler `gap_threshold`
    let howmany_blocks = 64;

    let chains = setup_chains(
        &node,
        /* single chain */ 1,
        howmany_blocks,
        &DEV_GENESIS_KEY,
        /* do not confirm */ false,
    );
    let (_, blocks) = chains.first().unwrap();

    // Confirm block towards at the beginning the chain, so gap between confirmation
    // and account frontier is larger than `gap_threshold`
    node.confirm(blocks[11].hash());

    // Ensure unconfirmed account head block gets activated
    let block = blocks.last().unwrap();
    assert_timely2(|| node.is_active_root(&block.qualified_root()));

    assert_eq!(
        node.active
            .read()
            .unwrap()
            .election_for_root(&block.qualified_root())
            .unwrap()
            .behavior(),
        ElectionBehavior::Optimistic
    );
}
