use std::time::Duration;

use tokio::time::sleep;
use tracing::{debug, info};

use rsnano_rpc_client::NanoRpcClient;
use rsnano_rpc_messages::{ReceiveArgs, SendArgs, WalletAddArgs, WalletRepresentativeSetArgs};
use rsnano_types::{
    Account, Amount, Block, BlockHash, JsonBlock, PrivateKey, PublicKey, StateBlockArgs, WalletId,
    WorkNonce,
};

use crate::{
    domain::{AccountMap, Representatives},
    setup::{genesis_key, pr_key},
};

/// The spam funds. Small against the supply: they delegate to no
/// representative, and their weight is a committee member with no node,
/// which a close's `n - f` would count against the honest reporters
const INITIAL_AMOUNT: Amount = Amount::nano(1_000_000);

/// The weight shared equally by the principal representatives; the rest funds the spam
pub(crate) fn voting_weight() -> Amount {
    Amount::MAX - INITIAL_AMOUNT
}

/// `balances` has an entry per principal representative, the offline and
/// Byzantine ones included: each holds its balance in the ledger, whether or
/// not it runs a node (PR0's entry is ignored: genesis keeps the rest).
/// `rpc_clients` are the nodes that do run, which get a wallet.
pub(crate) async fn create_wallets(
    rpc_clients: &[NanoRpcClient],
    genesis_rpc: &NanoRpcClient,
    account_map: &mut AccountMap,
    representatives: &Representatives,
    balances: &[Amount],
    prefund_count: usize,
) -> WalletId {
    let mut genesis_wallet = WalletId::ZERO;
    let genesis_key = genesis_key();
    let total_prs = balances.len();
    for (i, rpc_client) in rpc_clients.iter().enumerate() {
        info!("Creating wallet...");
        let resp = rpc_client.wallet_create(None).await.unwrap();
        if i == 0 {
            genesis_wallet = resp.wallet;
        }
        let pr_key = pr_key(i);
        rpc_client
            .wallet_add(WalletAddArgs {
                wallet: resp.wallet,
                key: pr_key.raw_key(),
                work: None,
            })
            .await
            .unwrap();

        info!("Setting default representative...");
        rpc_client
            .wallet_representative_set(WalletRepresentativeSetArgs {
                wallet: resp.wallet,
                representative: pr_key.account(),
                update_existing_accounts: Some(false.into()),
            })
            .await
            .unwrap();

        // the first rpc client is the genesis client
        if i > 0 {
            let pr_balance = balances[i];
            info!(
                "Sending Ӿ{} to PR{i} wallet {} ...",
                pr_balance.format_balance(0),
                pr_key.account().encode_account()
            );
            let send_hash = genesis_rpc
                .send(SendArgs {
                    wallet: genesis_wallet,
                    source: genesis_key.account(),
                    destination: pr_key.account(),
                    amount: pr_balance,
                    work: Some(WorkNonce::new(0)),
                    id: None,
                })
                .await
                .unwrap()
                .block;
            wait_until_confirmed(rpc_client, send_hash).await;

            info!("Receiving...");
            // trigger wallet receive to speed things up
            let _ = rpc_client
                .receive(ReceiveArgs {
                    wallet: resp.wallet,
                    account: pr_key.account(),
                    block: send_hash,
                    work: Some(WorkNonce::new(0)),
                })
                .await;
            let recv_hash = rpc_client
                .account_info(pr_key.account())
                .await
                .unwrap()
                .frontier;
            wait_until_confirmed(rpc_client, recv_hash).await;
            info!("DONE");
            info!(
                "********************************************************************************"
            );
        }
    }

    // The representatives without a node: funded the same way, but nanospam
    // has to create their receive block, since no wallet will
    for i in rpc_clients.len()..total_prs {
        let pr_key = pr_key(i);
        let pr_balance = balances[i];
        info!(
            "Sending \u{04FE}{} to PR{i} (no node) {} ...",
            pr_balance.format_balance(0),
            pr_key.account().encode_account()
        );
        let send_hash = genesis_rpc
            .send(SendArgs {
                wallet: genesis_wallet,
                source: genesis_key.account(),
                destination: pr_key.account(),
                amount: pr_balance,
                work: Some(WorkNonce::new(0)),
                id: None,
            })
            .await
            .unwrap()
            .block;
        wait_until_confirmed(genesis_rpc, send_hash).await;

        let receive: Block = StateBlockArgs {
            key: &pr_key,
            previous: BlockHash::ZERO,
            // A representative votes with its own weight
            representative: pr_key.public_key(),
            balance: pr_balance,
            link: send_hash.into(),
            work: 0.into(),
        }
        .into();
        let receive_hash = receive.hash();
        genesis_rpc.process(JsonBlock::from(receive)).await.unwrap();
        wait_until_confirmed(genesis_rpc, receive_hash).await;
        info!("DONE");
    }

    info!("Sending initial spam amount...");
    let initial_key = account_map.initial_key().clone();
    // Send total spam amount
    let genesis_send = genesis_rpc
        .send(SendArgs {
            wallet: genesis_wallet,
            source: genesis_key.account(),
            destination: initial_key.account(),
            amount: INITIAL_AMOUNT,
            work: Some(0.into()),
            id: None,
        })
        .await
        .unwrap()
        .block;
    wait_until_confirmed(genesis_rpc, genesis_send).await;
    info!("Receiving initial spam amount...");
    let initial_account = initial_key.account();
    // The initial account delegates to itself: its balance is the spam
    // funds, which would give one representative more weight than the rest
    let genesis_receive: Block = StateBlockArgs {
        key: &initial_key,
        previous: BlockHash::ZERO,
        representative: initial_key.public_key(),
        balance: INITIAL_AMOUNT,
        link: genesis_send.into(),
        work: 0.into(),
    }
    .into();

    let recv = genesis_rpc
        .process(JsonBlock::from(genesis_receive.clone()))
        .await
        .unwrap();

    wait_until_confirmed(genesis_rpc, recv.hash).await;

    account_map.set_account_state(initial_account, INITIAL_AMOUNT, genesis_receive.hash());

    let seeds = seed_representatives(genesis_rpc, account_map, representatives).await;
    prefund_accounts(
        genesis_rpc,
        account_map,
        representatives,
        &seeds,
        prefund_count,
    )
    .await;

    genesis_wallet
}

/// RAI: the representative an account delegates to; the account itself
/// in a run without representatives
fn representative_of(account: &Account, representatives: &Representatives) -> PublicKey {
    representatives
        .of(account)
        .unwrap_or_else(|| account.as_key())
}

/// RAI: the spam amount sits in one account, delegated to one
/// representative, which would hold it all in the committee derived from
/// epoch 0. Spread over one seed account per representative, every
/// representative starts with an equal share of it and the shares drift
/// from there with the spam.
async fn seed_representatives(
    genesis_rpc: &NanoRpcClient,
    account_map: &mut AccountMap,
    representatives: &Representatives,
) -> Vec<Account> {
    if representatives.len() < 2 {
        return Vec::new();
    }
    let initial_key = account_map.initial_key().clone();
    let initial_account = initial_key.account();
    let share = INITIAL_AMOUNT / (representatives.len() as u128 + 1);
    // The first account delegating to each representative, the initial
    // account's representative included: it keeps a share of its own
    let mut seeds: Vec<Account> = Vec::new();
    for rep in representatives.iter() {
        let seed = account_map
            .accounts()
            .iter()
            .skip(1)
            .find(|account| representatives.of(account) == Some(*rep) && !seeds.contains(account))
            .copied();
        if let Some(seed) = seed {
            seeds.push(seed);
        }
    }
    let mut frontier = account_map
        .state(&initial_account)
        .unwrap()
        .confirmed_frontier;
    let mut balance = INITIAL_AMOUNT;
    for seed in &seeds {
        balance -= share;
        let send = funding_send(
            &initial_key,
            frontier,
            initial_key.public_key(),
            balance,
            *seed,
        );
        frontier = send.hash();
        info!(
            "Seeding Ӿ{} to {} for representative {}",
            share.format_balance(0),
            seed.encode_account(),
            representative_of(seed, representatives)
        );
        genesis_rpc.process(JsonBlock::from(send)).await.unwrap();
        wait_until_confirmed(genesis_rpc, frontier).await;

        let receive = funding_receive(account_map, seed, frontier, share, representatives);
        let receive_hash = receive.hash();
        genesis_rpc.process(JsonBlock::from(receive)).await.unwrap();
        wait_until_confirmed(genesis_rpc, receive_hash).await;
        account_map.set_account_state(*seed, share, receive_hash);
    }
    account_map.set_account_state(initial_account, balance, frontier);
    seeds
}

fn funding_send(
    key: &PrivateKey,
    previous: BlockHash,
    representative: PublicKey,
    balance: Amount,
    destination: Account,
) -> Block {
    StateBlockArgs {
        key,
        previous,
        representative,
        balance,
        link: destination.into(),
        work: 0.into(),
    }
    .into()
}

/// The block that opens a funded account
fn funding_receive(
    account_map: &AccountMap,
    account: &Account,
    send: BlockHash,
    amount: Amount,
    representatives: &Representatives,
) -> Block {
    StateBlockArgs {
        key: &account_map.state(account).unwrap().key,
        previous: BlockHash::ZERO,
        representative: representative_of(account, representatives),
        balance: amount,
        link: send.into(),
        work: 0.into(),
    }
    .into()
}

/// An account can publish again only once its last block is confirmed, so
/// publishing `rate` blocks a second needs `rate * confirmation latency`
/// accounts ready to send. With too few, publishing follows the
/// confirmations instead of the rate at the start of the run. Each seed funds
/// accounts delegating to its own representative, which keeps every
/// representative's weight equal.
async fn prefund_accounts(
    genesis_rpc: &NanoRpcClient,
    account_map: &mut AccountMap,
    representatives: &Representatives,
    seeds: &[Account],
    count: usize,
) {
    if seeds.is_empty() {
        return;
    }
    let per_seed = (count / seeds.len()).saturating_sub(1);
    let groups: Vec<_> = seeds
        .iter()
        .map(|seed| {
            let targets = prefund_targets(account_map.accounts(), seed, representatives, per_seed);
            funding_rounds(*seed, &targets)
        })
        .collect();
    let round_count = groups.iter().map(Vec::len).max().unwrap_or(0);
    for round in 0..round_count {
        let transfers: Vec<_> = groups
            .iter()
            .filter_map(|rounds| rounds.get(round))
            .flatten()
            .copied()
            .collect();
        info!(
            "Prefunding round {} of {round_count}: {} accounts",
            round + 1,
            transfers.len()
        );
        fund_round(genesis_rpc, account_map, representatives, &transfers).await;
    }
}

/// Up to `count` accounts delegating to the seed's representative, without
/// the initial account and the seed
fn prefund_targets(
    accounts: &[Account],
    seed: &Account,
    representatives: &Representatives,
    count: usize,
) -> Vec<Account> {
    let rep = representatives.of(seed);
    accounts
        .iter()
        .skip(1)
        .filter(|account| *account != seed && representatives.of(account) == rep)
        .take(count)
        .copied()
        .collect()
}

/// Every funded account funds one more per round, so the setup takes
/// `log2(accounts)` rounds of confirmations instead of one per account.
/// Returns the (from, to) transfers of each round.
fn funding_rounds(seed: Account, targets: &[Account]) -> Vec<Vec<(Account, Account)>> {
    let mut funded = vec![seed];
    let mut remaining = targets;
    let mut rounds = Vec::new();
    while !remaining.is_empty() {
        let size = funded.len().min(remaining.len());
        let (next, rest) = remaining.split_at(size);
        rounds.push(funded.iter().copied().zip(next.iter().copied()).collect());
        funded.extend_from_slice(next);
        remaining = rest;
    }
    rounds
}

/// Half of each sender's balance moves to the funded account
async fn fund_round(
    genesis_rpc: &NanoRpcClient,
    account_map: &mut AccountMap,
    representatives: &Representatives,
    transfers: &[(Account, Account)],
) {
    let mut sends = Vec::with_capacity(transfers.len());
    for (from, to) in transfers {
        let state = account_map.state(from).unwrap();
        let amount = Amount::raw(state.balance.number() / 2);
        let balance = state.balance - amount;
        let send = funding_send(
            &state.key,
            state.confirmed_frontier,
            representative_of(from, representatives),
            balance,
            *to,
        );
        let send_hash = send.hash();
        genesis_rpc.process(JsonBlock::from(send)).await.unwrap();
        account_map.set_account_state(*from, balance, send_hash);
        sends.push((*to, send_hash, amount));
    }
    for (_, send_hash, _) in &sends {
        poll_until_confirmed(genesis_rpc, *send_hash).await;
    }

    let mut receives = Vec::with_capacity(sends.len());
    for (to, send_hash, amount) in sends {
        let receive = funding_receive(account_map, &to, send_hash, amount, representatives);
        let receive_hash = receive.hash();
        genesis_rpc.process(JsonBlock::from(receive)).await.unwrap();
        account_map.set_account_state(to, amount, receive_hash);
        receives.push(receive_hash);
    }
    for receive_hash in receives {
        poll_until_confirmed(genesis_rpc, receive_hash).await;
    }
}

pub(crate) async fn wait_until_confirmed(rpc_client: &NanoRpcClient, hash: BlockHash) {
    info!("Waiting for confirmation for {hash}");
    poll_until_confirmed(rpc_client, hash).await;
}

async fn poll_until_confirmed(rpc_client: &NanoRpcClient, hash: BlockHash) {
    loop {
        match rpc_client.block_info(hash).await {
            Ok(info) => {
                if info.confirmed.inner() {
                    break;
                }
            }
            Err(e) => {
                debug!("Got error: {e:?}")
            }
        }

        sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefund_targets_share_the_seeds_representative() {
        let reps = representatives(3);
        let accounts = accounts(60);
        let seed = accounts[1];

        let targets = prefund_targets(&accounts, &seed, &reps, 5);

        assert_eq!(targets.len(), 5);
        assert!(!targets.contains(&seed));
        assert!(!targets.contains(&accounts[0]));
        assert!(targets.iter().all(|t| reps.of(t) == reps.of(&seed)));
    }

    #[test]
    fn prefund_targets_stop_at_the_accounts_available() {
        let reps = representatives(3);
        let accounts = accounts(9);

        let targets = prefund_targets(&accounts, &accounts[1], &reps, 1000);

        assert!(targets.len() < 9);
    }

    #[test]
    fn funded_accounts_double_each_round() {
        let accounts = accounts(6);

        let rounds = funding_rounds(accounts[0], &accounts[1..]);

        let sizes: Vec<_> = rounds.iter().map(Vec::len).collect();
        assert_eq!(sizes, [1, 2, 2]);
    }

    #[test]
    fn every_target_is_funded_once_by_an_account_funded_earlier() {
        let accounts = accounts(9);

        let rounds = funding_rounds(accounts[0], &accounts[1..]);

        let mut funded = vec![accounts[0]];
        for round in &rounds {
            for (from, _) in round {
                assert!(funded.contains(from));
            }
            let mut senders: Vec<_> = round.iter().map(|(from, _)| *from).collect();
            senders.dedup();
            assert_eq!(senders.len(), round.len());
            funded.extend(round.iter().map(|(_, to)| *to));
        }
        assert_eq!(funded, accounts);
    }

    #[test]
    fn nothing_to_fund_takes_no_rounds() {
        assert!(funding_rounds(Account::from(1), &[]).is_empty());
    }

    /* Test helpers */

    fn accounts(count: u64) -> Vec<Account> {
        (1..=count).map(|i| PrivateKey::from(i).account()).collect()
    }

    fn representatives(count: u64) -> Representatives {
        Representatives::new(
            (100..100 + count)
                .map(|i| PrivateKey::from(i).public_key())
                .collect(),
        )
    }
}
