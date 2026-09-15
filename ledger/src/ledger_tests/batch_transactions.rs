use std::{sync::mpsc, time::Duration};

use rsnano_types::Block;

use crate::{
    BlockError, BlockSource, Ledger, LedgerBuilder, LedgerConstants, LedgerEvent, ProcessResult,
    test_helpers::UnsavedBlockLatticeBuilder,
};

fn assert_same_results(notified: &[ProcessResult], expected: &[ProcessResult]) {
    assert_eq!(notified.len(), expected.len());
    for (notified, expected) in notified.iter().zip(expected) {
        assert_eq!(notified.block, expected.block);
        assert_eq!(notified.source, expected.source);
        assert_eq!(notified.status, expected.status);
        assert_eq!(notified.saved_block, expected.saved_block);
        assert_eq!(notified.priority, expected.priority);
    }
}

/// Exercise real LMDB locking. Always release the held writer before joining,
/// so a regression reports a timeout rather than hanging the test process.
fn assert_completes_with_writer_held(name: &str, action: impl FnOnce(&Ledger) + Send) {
    let path = std::env::temp_dir().join(format!("ledger-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    let completed;
    let worker_result;
    {
        let ledger = LedgerBuilder::new(path.join("data.ldb"))
            .constants(LedgerConstants::dev())
            .init_thread_count(1)
            .finish()
            .unwrap();
        let writer = ledger.store.begin_write();
        (completed, worker_result) = std::thread::scope(|scope| {
            let (sender, receiver) = mpsc::channel();
            let ledger = &ledger;
            let worker = scope.spawn(move || {
                action(ledger);
                sender.send(()).unwrap();
            });
            let completed = receiver.recv_timeout(Duration::from_secs(2));
            drop(writer);
            let result = worker.join();
            (completed, result)
        });
    }
    std::fs::remove_dir_all(path).unwrap();
    worker_result.unwrap();
    assert!(completed.is_ok(), "operation waited for the LMDB writer");
}

#[test]
fn empty_competitor_batch_does_not_wait_for_writer() {
    assert_completes_with_writer_held("empty-competitors", |ledger| {
        let (sender, receiver) = mpsc::channel();
        *ledger.publish.write().unwrap() = Some(Box::new(move |event| {
            sender.send(event).unwrap();
        }));
        // Match the block processor's filtered iterator, which is empty for
        // normal batches even when the batch itself contains blocks.
        let block: Block = ledger.genesis().clone().into();
        ledger.roll_back_competitors(std::iter::once(&block).filter(|_| false));
        assert!(receiver.try_recv().is_err());
    });
}

#[test]
fn rejected_block_batch_does_not_wait_for_writer() {
    assert_completes_with_writer_held("rejected-batch", |ledger| {
        let (sender, receiver) = mpsc::channel();
        *ledger.publish.write().unwrap() = Some(Box::new(move |event| {
            sender.send(event).unwrap();
        }));
        let block: Block = ledger.genesis().clone().into();
        let result = ledger.process_batch([(&block, BlockSource::Live)]);
        assert_eq!(result.len(), 1);
        assert!(matches!(
            &result[0].status,
            Err(BlockError::Old(existing)) if existing.hash() == block.hash()
        ));
        assert_eq!(result[0].source, BlockSource::Live);
        assert!(result[0].saved_block.is_none());
        let LedgerEvent::BlocksProcessed(notified) = receiver.try_recv().unwrap() else {
            panic!("expected the rejected block notification");
        };
        assert_same_results(&notified, &result);
        assert!(receiver.try_recv().is_err());
    });
}

#[test]
fn mixed_block_batch_preserves_result_and_notification_order() {
    let ledger = Ledger::new_null();
    let (sender, receiver) = mpsc::channel();
    *ledger.publish.write().unwrap() = Some(Box::new(move |event| {
        sender.send(event).unwrap();
    }));
    let genesis: Block = ledger.genesis().clone().into();
    let mut lattice = UnsavedBlockLatticeBuilder::new();
    let first = lattice.genesis().send(1, 1);
    let second = lattice.genesis().send(2, 1);
    let result = ledger.process_batch([
        (&genesis, BlockSource::Live),
        (&first, BlockSource::Local),
        (&second, BlockSource::Bootstrap),
    ]);
    assert_eq!(result.len(), 3);
    assert!(matches!(result[0].status, Err(BlockError::Old(_))));
    assert_eq!(result[1].status, Ok(()));
    assert_eq!(result[1].saved_block.as_ref().unwrap().hash(), first.hash());
    // The whole batch still validates against its original read snapshot.
    assert_eq!(result[2].status, Err(BlockError::GapPrevious));
    assert_eq!(result[0].source, BlockSource::Live);
    assert_eq!(result[1].source, BlockSource::Local);
    assert_eq!(result[2].source, BlockSource::Bootstrap);
    let LedgerEvent::BlocksProcessed(notified) = receiver.try_recv().unwrap() else {
        panic!("expected the batch notification");
    };
    assert_same_results(&notified, &result);
    assert!(receiver.try_recv().is_err());
    assert!(ledger.process_one(&second).is_ok());
}
