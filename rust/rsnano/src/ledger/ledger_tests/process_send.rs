use rsnano_core::{Account, Amount, Epoch, PendingInfo, PendingKey};

use crate::{
    core::{Block, BlockDetails, BlockEnum},
    ledger::ledger_tests::{setup_send_block, LedgerContext},
    DEV_CONSTANTS, DEV_GENESIS_ACCOUNT,
};

#[test]
fn save_block() {
    let ctx = LedgerContext::empty();
    let mut txn = ctx.ledger.rw_txn();

    let send = setup_send_block(&ctx, txn.as_mut());

    let BlockEnum::State(loaded_block) = ctx.ledger.store.block().get(txn.txn(), &send.send_block.hash()).unwrap() else {panic!("not a state block")};
    assert_eq!(
        loaded_block.sideband().unwrap(),
        send.send_block.sideband().unwrap()
    );
    assert_eq!(loaded_block, send.send_block);
    assert_eq!(
        ctx.ledger.amount(txn.txn(), &send.send_block.hash()),
        Some(send.amount_sent)
    );
}

#[test]
fn update_pending_store() {
    let ctx = LedgerContext::empty();
    let mut txn = ctx.ledger.rw_txn();

    let send = setup_send_block(&ctx, txn.as_mut());

    let pending_info = ctx
        .ledger
        .get_pending(
            txn.txn(),
            &PendingKey::new(send.destination.account(), send.send_block.hash()),
        )
        .unwrap();

    assert_eq!(
        pending_info,
        PendingInfo {
            source: send.send_block.account(),
            amount: send.amount_sent,
            epoch: Epoch::Epoch0
        }
    );
}

#[test]
fn create_sideband() {
    let ctx = LedgerContext::empty();
    let mut txn = ctx.ledger.rw_txn();

    let send = setup_send_block(&ctx, txn.as_mut());

    let sideband = send.send_block.sideband().unwrap();
    assert_eq!(sideband.height, 2);
    assert_eq!(sideband.account, send.send_block.account());
    assert_eq!(
        sideband.details,
        BlockDetails::new(Epoch::Epoch0, true, false, false)
    );
}

#[test]
fn send_and_change_representative() {
    let ctx = LedgerContext::empty();
    let mut txn = ctx.ledger.rw_txn();
    let genesis = ctx.genesis_block_factory();
    let representative = Account::from(1);
    let amount_sent = DEV_CONSTANTS.genesis_amount - Amount::new(1);
    let mut send = genesis
        .send(txn.txn())
        .amount(amount_sent)
        .representative(representative)
        .build();
    ctx.ledger.process(txn.as_mut(), &mut send).unwrap();

    assert_eq!(
        ctx.ledger.amount(txn.txn(), &send.hash()).unwrap(),
        amount_sent,
    );
    assert_eq!(ctx.ledger.weight(&DEV_GENESIS_ACCOUNT), Amount::zero());
    assert_eq!(ctx.ledger.weight(&representative), Amount::new(1));
    assert_eq!(
        send.sideband().unwrap().details,
        BlockDetails::new(Epoch::Epoch0, true, false, false)
    );
}
