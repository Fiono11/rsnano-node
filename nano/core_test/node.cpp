#include "nano/secure/common.hpp"

#include <nano/lib/blocks.hpp>
#include <nano/lib/config.hpp>
#include <nano/lib/locks.hpp>
#include <nano/node/active_elections.hpp>
#include <nano/node/election.hpp>
#include <nano/node/inactive_node.hpp>
#include <nano/node/local_vote_history.hpp>
#include <nano/node/make_store.hpp>
#include <nano/node/scheduler/component.hpp>
#include <nano/node/scheduler/manual.hpp>
#include <nano/node/scheduler/priority.hpp>
#include <nano/node/transport/tcp_listener.hpp>
#include <nano/secure/ledger.hpp>
#include <nano/test_common/network.hpp>
#include <nano/test_common/system.hpp>
#include <nano/test_common/testutil.hpp>

#include <gtest/gtest.h>

#include <boost/filesystem.hpp>
#include <boost/make_shared.hpp>
#include <boost/optional.hpp>

#include <future>
#include <thread>

using namespace std::chrono_literals;

TEST (node, null_account)
{
	auto const & null_account = nano::account::null ();
	ASSERT_EQ (null_account, nullptr);
	ASSERT_FALSE (null_account != nullptr);

	nano::account default_account{};
	ASSERT_FALSE (default_account == nullptr);
	ASSERT_NE (default_account, nullptr);
}

TEST (node, stop)
{
	nano::test::system system (1);
	ASSERT_EQ (1, system.nodes[0]->wallets.wallet_count ());
	ASSERT_TRUE (true);
}

TEST (node, work_generate)
{
	nano::test::system system (1);
	auto & node (*system.nodes[0]);
	nano::block_hash root{ 1 };
	nano::work_version version{ nano::work_version::work_1 };
	{
		auto difficulty = nano::difficulty::from_multiplier (1.5, node.network_params.work.get_base ());
		auto work = node.work_generate_blocking (version, root, difficulty);
		ASSERT_TRUE (work.has_value ());
		ASSERT_GE (nano::dev::network_params.work.difficulty (version, root, work.value ()), difficulty);
	}
	{
		auto difficulty = nano::difficulty::from_multiplier (0.5, node.network_params.work.get_base ());
		std::optional<uint64_t> work;
		do
		{
			work = node.work_generate_blocking (version, root, difficulty);
		} while (nano::dev::network_params.work.difficulty (version, root, work.value ()) >= node.network_params.work.get_base ());
		ASSERT_TRUE (work.has_value ());
		ASSERT_GE (nano::dev::network_params.work.difficulty (version, root, work.value ()), difficulty);
		ASSERT_FALSE (nano::dev::network_params.work.difficulty (version, root, work.value ()) >= node.network_params.work.get_base ());
	}
}

TEST (node, block_store_path_failure)
{
	nano::test::system system;
	auto service (boost::make_shared<rsnano::async_runtime> (false));
	auto path (nano::unique_path ());
	nano::work_pool pool{ nano::dev::network_params.network, std::numeric_limits<unsigned>::max () };
	auto node (std::make_shared<nano::node> (*service, system.get_available_port (), path, pool));
	system.register_node (node);
	ASSERT_EQ (0, node->wallets.wallet_count ());
}

TEST (node, balance)
{
	nano::test::system system (1);
	auto node = system.nodes[0];
	auto wallet_id = node->wallets.first_wallet_id ();
	(void)node->wallets.insert_adhoc (wallet_id, nano::dev::genesis_key.prv);
	auto transaction (node->store.tx_begin_write ());
	ASSERT_EQ (std::numeric_limits<nano::uint128_t>::max (), node->ledger.any ().account_balance (*transaction, nano::dev::genesis_key.pub).value ().number ());
}

TEST (node, send_unkeyed)
{
	nano::test::system system (1);
	auto node = system.nodes[0];
	auto wallet_id = node->wallets.first_wallet_id ();
	nano::keypair key2;
	(void)node->wallets.insert_adhoc (wallet_id, nano::dev::genesis_key.prv);
	node->wallets.set_password (wallet_id, nano::keypair ().prv);
	ASSERT_EQ (nullptr, node->wallets.send_action (wallet_id, nano::dev::genesis_key.pub, key2.pub, node->config->receive_minimum.number ()));
}

TEST (node, send_self)
{
	nano::test::system system (1);
	nano::keypair key2;
	auto node = system.nodes[0];
	auto wallet_id = node->wallets.first_wallet_id ();
	(void)node->wallets.insert_adhoc (wallet_id, nano::dev::genesis_key.prv);
	(void)node->wallets.insert_adhoc (wallet_id, key2.prv);
	ASSERT_NE (nullptr, node->wallets.send_action (wallet_id, nano::dev::genesis_key.pub, key2.pub, node->config->receive_minimum.number ()));
	ASSERT_TIMELY (10s, !node->balance (key2.pub).is_zero ());
	ASSERT_EQ (std::numeric_limits<nano::uint128_t>::max () - node->config->receive_minimum.number (), node->balance (nano::dev::genesis_key.pub));
}

TEST (node, send_single)
{
	nano::test::system system (2);
	nano::keypair key2;
	auto node1 = system.nodes[0];
	auto node2 = system.nodes[1];
	auto wallet_id1 = node1->wallets.first_wallet_id ();
	auto wallet_id2 = node2->wallets.first_wallet_id ();
	(void)node1->wallets.insert_adhoc (wallet_id1, nano::dev::genesis_key.prv);
	(void)node2->wallets.insert_adhoc (wallet_id2, key2.prv);
	ASSERT_NE (nullptr, node1->wallets.send_action (wallet_id1, nano::dev::genesis_key.pub, key2.pub, node1->config->receive_minimum.number ()));
	ASSERT_EQ (std::numeric_limits<nano::uint128_t>::max () - node1->config->receive_minimum.number (), node1->balance (nano::dev::genesis_key.pub));
	ASSERT_TRUE (node1->balance (key2.pub).is_zero ());
	ASSERT_TIMELY (10s, !node1->balance (key2.pub).is_zero ());
}

TEST (node, send_single_observing_peer)
{
	nano::test::system system (3);
	nano::keypair key2;
	auto node1 = system.nodes[0];
	auto node2 = system.nodes[1];
	auto wallet_id1 = node1->wallets.first_wallet_id ();
	auto wallet_id2 = node2->wallets.first_wallet_id ();
	(void)node1->wallets.insert_adhoc (wallet_id1, nano::dev::genesis_key.prv);
	(void)node2->wallets.insert_adhoc (wallet_id2, key2.prv);
	ASSERT_NE (nullptr, node1->wallets.send_action (wallet_id1, nano::dev::genesis_key.pub, key2.pub, node1->config->receive_minimum.number ()));
	ASSERT_EQ (std::numeric_limits<nano::uint128_t>::max () - node1->config->receive_minimum.number (), node1->balance (nano::dev::genesis_key.pub));
	ASSERT_TRUE (node1->balance (key2.pub).is_zero ());
	ASSERT_TIMELY (10s, std::all_of (system.nodes.begin (), system.nodes.end (), [&] (std::shared_ptr<nano::node> const & node_a) { return !node_a->balance (key2.pub).is_zero (); }));
}

TEST (node, send_out_of_order)
{
	nano::test::system system (2);
	auto & node1 (*system.nodes[0]);
	nano::keypair key2;
	nano::send_block_builder builder;
	auto send1 = builder.make_block ()
				 .previous (nano::dev::genesis->hash ())
				 .destination (key2.pub)
				 .balance (std::numeric_limits<nano::uint128_t>::max () - node1.config->receive_minimum.number ())
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (nano::dev::genesis->hash ()))
				 .build ();
	auto send2 = builder.make_block ()
				 .previous (send1->hash ())
				 .destination (key2.pub)
				 .balance (std::numeric_limits<nano::uint128_t>::max () - 2 * node1.config->receive_minimum.number ())
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (send1->hash ()))
				 .build ();
	auto send3 = builder.make_block ()
				 .previous (send2->hash ())
				 .destination (key2.pub)
				 .balance (std::numeric_limits<nano::uint128_t>::max () - 3 * node1.config->receive_minimum.number ())
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (send2->hash ()))
				 .build ();
	node1.process_active (send3);
	node1.process_active (send2);
	node1.process_active (send1);
	ASSERT_TIMELY (10s, std::all_of (system.nodes.begin (), system.nodes.end (), [&] (std::shared_ptr<nano::node> const & node_a) { return node_a->balance (nano::dev::genesis_key.pub) == nano::dev::constants.genesis_amount - node1.config->receive_minimum.number () * 3; }));
}

TEST (node, quick_confirm)
{
	nano::test::system system (1);
	auto & node1 (*system.nodes[0]);
	auto wallet_id = node1.wallets.first_wallet_id ();
	nano::keypair key;
	nano::block_hash previous (node1.latest (nano::dev::genesis_key.pub));
	auto genesis_start_balance (node1.balance (nano::dev::genesis_key.pub));
	(void)node1.wallets.insert_adhoc (wallet_id, key.prv);
	(void)node1.wallets.insert_adhoc (wallet_id, nano::dev::genesis_key.prv);
	auto send = nano::send_block_builder ()
				.previous (previous)
				.destination (key.pub)
				.balance (node1.quorum ().quorum_delta.number () + 1)
				.sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				.work (*system.work.generate (previous))
				.build ();
	node1.process_active (send);
	ASSERT_TIMELY (10s, !node1.balance (key.pub).is_zero ());
	ASSERT_EQ (node1.balance (nano::dev::genesis_key.pub), node1.quorum ().quorum_delta.number () + 1);
	ASSERT_EQ (node1.balance (key.pub), genesis_start_balance - (node1.quorum ().quorum_delta.number () + 1));
}

TEST (node, node_receive_quorum)
{
	nano::test::system system (1);
	auto & node1 = *system.nodes[0];
	auto wallet_id = node1.wallets.first_wallet_id ();
	nano::keypair key;
	nano::block_hash previous (node1.latest (nano::dev::genesis_key.pub));
	(void)node1.wallets.insert_adhoc (wallet_id, key.prv);
	auto send = nano::send_block_builder ()
				.previous (previous)
				.destination (key.pub)
				.balance (nano::dev::constants.genesis_amount - nano::Gxrb_ratio)
				.sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				.work (*system.work.generate (previous))
				.build ();
	node1.process_active (send);
	ASSERT_TIMELY (10s, node1.block_or_pruned_exists (send->hash ()));
	ASSERT_TIMELY (10s, node1.active.election (nano::qualified_root (previous, previous)) != nullptr);
	auto election (node1.active.election (nano::qualified_root (previous, previous)));
	ASSERT_NE (nullptr, election);
	ASSERT_FALSE (node1.active.confirmed (*election));
	ASSERT_EQ (1, election->votes ().size ());

	nano::test::system system2;
	system2.add_node ();
	auto node2 = system2.nodes[0];
	auto wallet_id2 = node2->wallets.first_wallet_id ();

	(void)node2->wallets.insert_adhoc (wallet_id2, nano::dev::genesis_key.prv);
	ASSERT_TRUE (node1.balance (key.pub).is_zero ());
	node1.connect (node2->network->endpoint ());
	while (node1.balance (key.pub).is_zero ())
	{
		ASSERT_NO_ERROR (system.poll ());
		ASSERT_NO_ERROR (system2.poll ());
	}
}

TEST (node, auto_bootstrap)
{
	nano::test::system system;
	nano::node_config config (system.get_available_port ());
	config.frontiers_confirmation = nano::frontiers_confirmation_mode::disabled;
	nano::node_flags node_flags;
	node_flags.set_disable_bootstrap_bulk_push_client (true);
	node_flags.set_disable_lazy_bootstrap (true);
	auto node0 = system.add_node (config, node_flags);
	auto wallet_id = node0->wallets.first_wallet_id ();
	nano::keypair key2;
	(void)node0->wallets.insert_adhoc (wallet_id, nano::dev::genesis_key.prv);
	(void)node0->wallets.insert_adhoc (wallet_id, key2.prv);
	auto send1 (node0->wallets.send_action (wallet_id, nano::dev::genesis_key.pub, key2.pub, node0->config->receive_minimum.number ()));
	ASSERT_NE (nullptr, send1);
	ASSERT_TIMELY_EQ (10s, node0->balance (key2.pub), node0->config->receive_minimum.number ());
	auto node1 (std::make_shared<nano::node> (system.async_rt, system.get_available_port (), nano::unique_path (), system.work, node_flags));
	ASSERT_FALSE (node1->init_error ());
	node1->start ();
	system.nodes.push_back (node1);
	nano::test::establish_tcp (system, *node1, node0->network->endpoint ());
	ASSERT_TIMELY_EQ (10s, node1->balance (key2.pub), node0->config->receive_minimum.number ());
	// ASSERT_TIMELY (10s, !node1->bootstrap_initiator.in_progress ());
	ASSERT_TRUE (node1->block_or_pruned_exists (send1->hash ()));
	// Wait block receive
	ASSERT_TIMELY_EQ (5s, node1->ledger.block_count (), 3);
	// Confirmation for all blocks
	ASSERT_TIMELY_EQ (5s, node1->ledger.cemented_count (), 3);
}

TEST (node, auto_bootstrap_reverse)
{
	nano::test::system system;
	nano::node_config config (system.get_available_port ());
	config.frontiers_confirmation = nano::frontiers_confirmation_mode::disabled;
	nano::node_flags node_flags;
	node_flags.set_disable_bootstrap_bulk_push_client (true);
	node_flags.set_disable_lazy_bootstrap (true);
	auto node0 = system.add_node (config, node_flags);
	auto wallet_id = node0->wallets.first_wallet_id ();
	nano::keypair key2;
	(void)node0->wallets.insert_adhoc (wallet_id, nano::dev::genesis_key.prv);
	(void)node0->wallets.insert_adhoc (wallet_id, key2.prv);
	auto node1 (std::make_shared<nano::node> (system.async_rt, system.get_available_port (), nano::unique_path (), system.work, node_flags));
	ASSERT_FALSE (node1->init_error ());
	ASSERT_NE (nullptr, node0->wallets.send_action (wallet_id, nano::dev::genesis_key.pub, key2.pub, node0->config->receive_minimum.number ()));
	node1->start ();
	system.nodes.push_back (node1);
	nano::test::establish_tcp (system, *node0, node1->network->endpoint ());
	ASSERT_TIMELY_EQ (10s, node1->balance (key2.pub), node0->config->receive_minimum.number ());
}

TEST (node, auto_bootstrap_age)
{
	nano::test::system system;
	nano::node_config config (system.get_available_port ());
	config.frontiers_confirmation = nano::frontiers_confirmation_mode::disabled;
	nano::node_flags node_flags;
	node_flags.set_disable_bootstrap_bulk_push_client (true);
	node_flags.set_disable_lazy_bootstrap (true);
	node_flags.set_bootstrap_interval (1);
	auto node0 = system.add_node (config, node_flags);
	auto node1 (std::make_shared<nano::node> (system.async_rt, system.get_available_port (), nano::unique_path (), system.work, node_flags));
	ASSERT_FALSE (node1->init_error ());
	node1->start ();
	system.nodes.push_back (node1);
	nano::test::establish_tcp (system, *node1, node0->network->endpoint ());
	//  4 bootstraps with frontiers age
	ASSERT_TIMELY (10s, node0->stats->count (nano::stat::type::bootstrap, nano::stat::detail::initiate_legacy_age, nano::stat::dir::out) >= 3);
	// More attempts with frontiers age
	ASSERT_GE (node0->stats->count (nano::stat::type::bootstrap, nano::stat::detail::initiate_legacy_age, nano::stat::dir::out), node0->stats->count (nano::stat::type::bootstrap, nano::stat::detail::initiate, nano::stat::dir::out));
}

TEST (node, merge_peers)
{
	nano::test::system system (1);
	std::array<nano::endpoint, 8> endpoints;
	endpoints.fill (nano::endpoint (boost::asio::ip::address_v6::loopback (), system.get_available_port ()));
	endpoints[0] = nano::endpoint (boost::asio::ip::address_v6::loopback (), system.get_available_port ());
	system.nodes[0]->network->merge_peers (endpoints);
	ASSERT_EQ (0, system.nodes[0]->network->size ());
}

TEST (node, search_receivable)
{
	nano::test::system system (1);
	auto node (system.nodes[0]);
	auto wallet_id = node->wallets.first_wallet_id ();
	nano::keypair key2;
	(void)node->wallets.insert_adhoc (wallet_id, nano::dev::genesis_key.prv);
	ASSERT_NE (nullptr, node->wallets.send_action (wallet_id, nano::dev::genesis_key.pub, key2.pub, node->config->receive_minimum.number ()));
	(void)node->wallets.insert_adhoc (wallet_id, key2.prv);
	ASSERT_EQ (nano::wallets_error::none, node->wallets.search_receivable (wallet_id));
	ASSERT_TIMELY (10s, !node->balance (key2.pub).is_zero ());
}

TEST (node, search_receivable_same)
{
	nano::test::system system (1);
	auto node (system.nodes[0]);
	auto wallet_id = node->wallets.first_wallet_id ();
	nano::keypair key2;
	(void)node->wallets.insert_adhoc (wallet_id, nano::dev::genesis_key.prv);
	ASSERT_NE (nullptr, node->wallets.send_action (wallet_id, nano::dev::genesis_key.pub, key2.pub, node->config->receive_minimum.number ()));
	ASSERT_NE (nullptr, node->wallets.send_action (wallet_id, nano::dev::genesis_key.pub, key2.pub, node->config->receive_minimum.number ()));
	(void)node->wallets.insert_adhoc (wallet_id, key2.prv);
	ASSERT_EQ (nano::wallets_error::none, node->wallets.search_receivable (wallet_id));
	ASSERT_TIMELY_EQ (10s, node->balance (key2.pub), 2 * node->config->receive_minimum.number ());
}

TEST (node, search_receivable_multiple)
{
	nano::test::system system (1);
	auto node (system.nodes[0]);
	auto wallet_id = node->wallets.first_wallet_id ();
	nano::keypair key2;
	nano::keypair key3;
	(void)node->wallets.insert_adhoc (wallet_id, nano::dev::genesis_key.prv);
	(void)node->wallets.insert_adhoc (wallet_id, key3.prv);
	ASSERT_NE (nullptr, node->wallets.send_action (wallet_id, nano::dev::genesis_key.pub, key3.pub, node->config->receive_minimum.number ()));
	ASSERT_TIMELY (10s, !node->balance (key3.pub).is_zero ());
	ASSERT_NE (nullptr, node->wallets.send_action (wallet_id, nano::dev::genesis_key.pub, key2.pub, node->config->receive_minimum.number ()));
	ASSERT_NE (nullptr, node->wallets.send_action (wallet_id, key3.pub, key2.pub, node->config->receive_minimum.number ()));
	(void)node->wallets.insert_adhoc (wallet_id, key2.prv);
	ASSERT_EQ (nano::wallets_error::none, node->wallets.search_receivable (wallet_id));
	ASSERT_TIMELY_EQ (10s, node->balance (key2.pub), 2 * node->config->receive_minimum.number ());
}

TEST (node, search_receivable_confirmed)
{
	nano::test::system system;
	nano::node_config node_config (system.get_available_port ());
	node_config.frontiers_confirmation = nano::frontiers_confirmation_mode::disabled;
	auto node = system.add_node (node_config);
	auto wallet_id = node->wallets.first_wallet_id ();
	nano::keypair key2;
	(void)node->wallets.insert_adhoc (wallet_id, nano::dev::genesis_key.prv);

	auto send1 (node->wallets.send_action (wallet_id, nano::dev::genesis_key.pub, key2.pub, node->config->receive_minimum.number ()));
	ASSERT_NE (nullptr, send1);
	ASSERT_TIMELY (5s, nano::test::confirmed (*node, { send1 }));

	auto send2 (node->wallets.send_action (wallet_id, nano::dev::genesis_key.pub, key2.pub, node->config->receive_minimum.number ()));
	ASSERT_NE (nullptr, send2);
	ASSERT_TIMELY (5s, nano::test::confirmed (*node, { send2 }));

	ASSERT_EQ (nano::wallets_error::none, node->wallets.remove_account (wallet_id, nano::dev::genesis_key.pub));

	(void)node->wallets.insert_adhoc (wallet_id, key2.prv);
	ASSERT_EQ (nano::wallets_error::none, node->wallets.search_receivable (wallet_id));
	ASSERT_TIMELY (5s, !node->election_active (send1->hash ()));
	ASSERT_TIMELY (5s, !node->election_active (send2->hash ()));
	ASSERT_TIMELY_EQ (5s, node->balance (key2.pub), 2 * node->config->receive_minimum.number ());
}

TEST (node, search_receivable_pruned)
{
	nano::test::system system;
	nano::node_config node_config (system.get_available_port ());
	node_config.frontiers_confirmation = nano::frontiers_confirmation_mode::disabled;
	auto node1 = system.add_node (node_config);
	auto wallet_id = node1->wallets.first_wallet_id ();
	nano::node_flags node_flags;
	node_flags.set_enable_pruning (true);
	nano::node_config config (system.get_available_port ());
	config.enable_voting = false; // Remove after allowing pruned voting
	auto node2 = system.add_node (config, node_flags);
	auto wallet_id2 = node2->wallets.first_wallet_id ();
	nano::keypair key2;
	(void)node1->wallets.insert_adhoc (wallet_id, nano::dev::genesis_key.prv);
	auto send1 (node1->wallets.send_action (wallet_id, nano::dev::genesis_key.pub, key2.pub, node2->config->receive_minimum.number ()));
	ASSERT_NE (nullptr, send1);
	auto send2 (node1->wallets.send_action (wallet_id, nano::dev::genesis_key.pub, key2.pub, node2->config->receive_minimum.number ()));
	ASSERT_NE (nullptr, send2);

	// Confirmation
	ASSERT_TIMELY (10s, node1->active.empty () && node2->active.empty ());
	ASSERT_TIMELY (5s, node1->ledger.confirmed ().block_exists (*node1->store.tx_begin_read (), send2->hash ()));
	ASSERT_TIMELY_EQ (5s, node2->ledger.cemented_count (), 3);
	ASSERT_EQ (nano::wallets_error::none, node1->wallets.remove_account (wallet_id, nano::dev::genesis_key.pub));

	// Pruning
	{
		auto transaction (node2->store.tx_begin_write ());
		ASSERT_EQ (1, node2->ledger.pruning_action (*transaction, send1->hash (), 1));
	}
	ASSERT_EQ (1, node2->ledger.pruned_count ());
	ASSERT_TRUE (node2->block_or_pruned_exists (send1->hash ())); // true for pruned

	// Receive pruned block
	(void)node2->wallets.insert_adhoc (wallet_id2, key2.prv);
	ASSERT_EQ (nano::wallets_error::none, node2->wallets.search_receivable (wallet_id2));
	ASSERT_TIMELY_EQ (10s, node2->balance (key2.pub), 2 * node2->config->receive_minimum.number ());
}

TEST (node, unlock_search)
{
	nano::test::system system (1);
	auto node (system.nodes[0]);
	auto wallet_id = node->wallets.first_wallet_id ();
	nano::keypair key2;
	nano::uint128_t balance (node->balance (nano::dev::genesis_key.pub));
	ASSERT_EQ (nano::wallets_error::none, node->wallets.rekey (wallet_id, ""));
	(void)node->wallets.insert_adhoc (wallet_id, nano::dev::genesis_key.prv);
	ASSERT_NE (nullptr, node->wallets.send_action (wallet_id, nano::dev::genesis_key.pub, key2.pub, node->config->receive_minimum.number ()));
	ASSERT_TIMELY (10s, node->balance (nano::dev::genesis_key.pub) != balance);
	ASSERT_TIMELY (10s, node->active.empty ());
	(void)node->wallets.insert_adhoc (wallet_id, key2.prv);
	node->wallets.set_password (wallet_id, nano::keypair ().prv);
	ASSERT_EQ (nano::wallets_error::none, node->wallets.enter_password (wallet_id, ""));
	ASSERT_TIMELY (10s, !node->balance (key2.pub).is_zero ());
}

TEST (node, working)
{
	auto path (nano::working_path ());
	ASSERT_FALSE (path.empty ());
}

TEST (node_config, random_rep)
{
	auto path (nano::unique_path ());
	nano::node_config config1 (100);
	auto rep (config1.random_representative ());
	ASSERT_NE (config1.preconfigured_representatives.end (), std::find (config1.preconfigured_representatives.begin (), config1.preconfigured_representatives.end (), rep));
}

TEST (node, expire)
{
	std::weak_ptr<nano::node> node0;
	{
		nano::test::system system (1);
		node0 = system.nodes[0];
		auto wallet_id0 = system.nodes[0]->wallets.first_wallet_id ();
		auto & node1 (*system.nodes[0]);
		auto wallet_id1 = node1.wallets.first_wallet_id ();
		(void)system.nodes[0]->wallets.insert_adhoc (wallet_id0, nano::dev::genesis_key.prv);
	}
	ASSERT_TRUE (node0.expired ());
}

// In test case there used to be a race condition, it was worked around in:.
// https://github.com/nanocurrency/nano-node/pull/4091
// The election and the processing of block send2 happen in parallel.
// Usually the election happens first and the send2 block is added to the election.
// However, if the send2 block is processed before the election is started then
// there is a race somewhere and the election might not notice the send2 block.
// The test case can be made to pass by ensuring the election is started before the send2 is processed.
// However, is this a problem with the test case or this is a problem with the node handling of forks?
TEST (node, fork_publish_inactive)
{
	nano::test::system system (1);
	auto & node = *system.nodes[0];
	nano::keypair key1;
	nano::keypair key2;

	nano::send_block_builder builder;

	auto send1 = builder.make_block ()
				 .previous (nano::dev::genesis->hash ())
				 .destination (key1.pub)
				 .balance (nano::dev::constants.genesis_amount - 100)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (nano::dev::genesis->hash ()))
				 .build ();

	auto send2 = builder.make_block ()
				 .previous (nano::dev::genesis->hash ())
				 .destination (key2.pub)
				 .balance (nano::dev::constants.genesis_amount - 100)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (send1->block_work ())
				 .build ();

	node.process_active (send1);
	ASSERT_TIMELY (5s, node.block (send1->hash ()));

	std::shared_ptr<nano::election> election;
	ASSERT_TIMELY (5s, election = node.active.election (send1->qualified_root ()));

	ASSERT_EQ (nano::block_status::fork, node.process_local (send2).value ());

	ASSERT_TIMELY_EQ (5s, election->blocks ().size (), 2);

	auto find_block = [&election] (nano::block_hash hash_a) -> bool {
		auto blocks = election->blocks ();
		return blocks.end () != blocks.find (hash_a);
	};
	ASSERT_TRUE (find_block (send1->hash ()));
	ASSERT_TRUE (find_block (send2->hash ()));

	ASSERT_EQ (election->winner ()->hash (), send1->hash ());
	ASSERT_NE (election->winner ()->hash (), send2->hash ());
}

TEST (node, fork_keep)
{
	nano::test::system system (2);
	auto & node1 (*system.nodes[0]);
	auto & node2 (*system.nodes[1]);
	auto wallet_id1 = node1.wallets.first_wallet_id ();
	ASSERT_EQ (1, node1.network->size ());
	nano::keypair key1;
	nano::keypair key2;
	nano::send_block_builder builder;
	// send1 and send2 fork to different accounts
	auto send1 = builder.make_block ()
				 .previous (nano::dev::genesis->hash ())
				 .destination (key1.pub)
				 .balance (nano::dev::constants.genesis_amount - 100)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (nano::dev::genesis->hash ()))
				 .build ();
	auto send2 = builder.make_block ()
				 .previous (nano::dev::genesis->hash ())
				 .destination (key2.pub)
				 .balance (nano::dev::constants.genesis_amount - 100)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (nano::dev::genesis->hash ()))
				 .build ();
	node1.process_active (send1);
	node2.process_active (builder.make_block ().from (*send1).build ());
	ASSERT_TIMELY_EQ (5s, 1, node1.active.size ());
	ASSERT_TIMELY_EQ (5s, 1, node2.active.size ());
	(void)node1.wallets.insert_adhoc (wallet_id1, nano::dev::genesis_key.prv);
	// Fill node with forked blocks
	node1.process_active (send2);
	ASSERT_TIMELY (5s, node1.active.active (*send2));
	node2.process_active (builder.make_block ().from (*send2).build ());
	ASSERT_TIMELY (5s, node2.active.active (*send2));
	auto election1 (node2.active.election (nano::qualified_root (nano::dev::genesis->hash (), nano::dev::genesis->hash ())));
	ASSERT_NE (nullptr, election1);
	ASSERT_EQ (1, election1->votes ().size ());
	ASSERT_TRUE (node1.block_or_pruned_exists (send1->hash ()));
	ASSERT_TRUE (node2.block_or_pruned_exists (send1->hash ()));
	// Wait until the genesis rep makes a vote
	ASSERT_TIMELY (1.5min, election1->votes ().size () != 1);
	auto transaction0 (node1.store.tx_begin_read ());
	auto transaction1 (node2.store.tx_begin_read ());
	// The vote should be in agreement with what we already have.
	auto winner (*node2.active.tally (*election1).begin ());
	ASSERT_EQ (*send1, *winner.second);
	ASSERT_EQ (nano::dev::constants.genesis_amount - 100, winner.first);
	ASSERT_TRUE (node1.ledger.any ().block_exists (*transaction0, send1->hash ()));
	ASSERT_TRUE (node2.ledger.any ().block_exists (*transaction1, send1->hash ()));
}

// Test that more than one block can be rolled back
TEST (node, fork_multi_flip)
{
	auto type = nano::transport::transport_type::tcp;
	nano::test::system system;
	nano::node_flags node_flags;
	nano::node_config node_config (system.get_available_port ());
	node_config.frontiers_confirmation = nano::frontiers_confirmation_mode::disabled;
	auto & node1 (*system.add_node (node_config, node_flags, type));
	auto wallet_id1 = node1.wallets.first_wallet_id ();
	node_config.peering_port = system.get_available_port ();
	auto & node2 (*system.add_node (node_config, node_flags, type));
	ASSERT_EQ (1, node1.network->size ());
	nano::keypair key1;
	nano::send_block_builder builder;
	auto send1 = builder.make_block ()
				 .previous (nano::dev::genesis->hash ())
				 .destination (key1.pub)
				 .balance (nano::dev::constants.genesis_amount - 100)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (nano::dev::genesis->hash ()))
				 .build ();
	nano::keypair key2;
	auto send2 = builder.make_block ()
				 .previous (nano::dev::genesis->hash ())
				 .destination (key2.pub)
				 .balance (nano::dev::constants.genesis_amount - 100)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (nano::dev::genesis->hash ()))
				 .build ();
	auto send3 = builder.make_block ()
				 .previous (send2->hash ())
				 .destination (key2.pub)
				 .balance (nano::dev::constants.genesis_amount - 100)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (send2->hash ()))
				 .build ();
	ASSERT_EQ (nano::block_status::progress, node1.ledger.process (*node1.store.tx_begin_write (), send1));
	// Node2 has two blocks that will be rolled back by node1's vote
	ASSERT_EQ (nano::block_status::progress, node2.ledger.process (*node2.store.tx_begin_write (), send2));
	ASSERT_EQ (nano::block_status::progress, node2.ledger.process (*node2.store.tx_begin_write (), send3));
	(void)node1.wallets.insert_adhoc (wallet_id1, nano::dev::genesis_key.prv); // Insert voting key in to node1

	auto election = nano::test::start_election (system, node2, send2->hash ());
	ASSERT_NE (nullptr, election);
	ASSERT_TIMELY (5s, election->contains (send1->hash ()));
	nano::test::confirm (node1.ledger, send1);
	ASSERT_TIMELY (10s, node2.block_or_pruned_exists (send1->hash ()));
	ASSERT_TRUE (nano::test::block_or_pruned_none_exists (node2, { send2, send3 }));
	auto winner = election->winner ();
	ASSERT_EQ (*send1, *winner);
	ASSERT_EQ (nano::dev::constants.genesis_amount - 100, election->get_status ().get_tally ().number ());
}

// Blocks that are no longer actively being voted on should be able to be evicted through bootstrapping.
// This could happen if a fork wasn't resolved before the process previously shut down
TEST (node, fork_bootstrap_flip)
{
	nano::test::system system;
	nano::node_config config0{ system.get_available_port () };
	config0.frontiers_confirmation = nano::frontiers_confirmation_mode::disabled;
	nano::node_flags node_flags;
	node_flags.set_disable_bootstrap_bulk_push_client (true);
	node_flags.set_disable_lazy_bootstrap (true);
	auto & node1 = *system.add_node (config0, node_flags);
	auto wallet_id1 = node1.wallets.first_wallet_id ();
	(void)node1.wallets.insert_adhoc (wallet_id1, nano::dev::genesis_key.prv);
	nano::node_config config1 (system.get_available_port ());
	auto & node2 = *system.make_disconnected_node (config1, node_flags);
	(void)node1.wallets.insert_adhoc (wallet_id1, nano::dev::genesis_key.prv);
	nano::block_hash latest = node1.latest (nano::dev::genesis_key.pub);
	nano::keypair key1;
	nano::send_block_builder builder;
	auto send1 = builder.make_block ()
				 .previous (latest)
				 .destination (key1.pub)
				 .balance (nano::dev::constants.genesis_amount - nano::Gxrb_ratio)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (latest))
				 .build ();
	nano::keypair key2;
	auto send2 = builder.make_block ()
				 .previous (latest)
				 .destination (key2.pub)
				 .balance (nano::dev::constants.genesis_amount - nano::Gxrb_ratio)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (latest))
				 .build ();
	// Insert but don't rebroadcast, simulating settled blocks
	{
		auto tx{ node1.store.tx_begin_write () };
		ASSERT_EQ (nano::block_status::progress, node1.ledger.process (*tx, send1));
	}
	{
		auto tx{ node2.store.tx_begin_write () };
		ASSERT_EQ (nano::block_status::progress, node2.ledger.process (*tx, send2));
	}

	nano::test::confirm (node1.ledger, send1);
	ASSERT_TIMELY (1s, node1.ledger.any ().block_exists (*node1.ledger.store.tx_begin_read (), send1->hash ()));
	ASSERT_TIMELY (1s, node2.ledger.any ().block_exists (*node2.ledger.store.tx_begin_read (), send2->hash ()));

	// Additionally add new peer to confirm & replace bootstrap block
	node2.network->merge_peer (node1.network->endpoint ());

	ASSERT_TIMELY (10s, node2.ledger.any ().block_exists (*node2.ledger.store.tx_begin_read (), send1->hash ()));
}

TEST (node, fork_open_flip)
{
	nano::test::system system (1);
	auto & node1 = *system.nodes[0];
	auto wallet_id = node1.wallets.first_wallet_id ();

	std::shared_ptr<nano::election> election;
	nano::keypair key1;
	nano::keypair rep1;
	nano::keypair rep2;

	// send 1 raw from genesis to key1 on both node1 and node2
	auto send1 = nano::send_block_builder ()
				 .previous (nano::dev::genesis->hash ())
				 .destination (key1.pub)
				 .balance (nano::dev::constants.genesis_amount - 1)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (nano::dev::genesis->hash ()))
				 .build ();
	node1.process_active (send1);

	// We should be keeping this block
	nano::open_block_builder builder;
	auto open1 = builder.make_block ()
				 .source (send1->hash ())
				 .representative (rep1.pub)
				 .account (key1.pub)
				 .sign (key1.prv, key1.pub)
				 .work (*system.work.generate (key1.pub))
				 .build ();

	// create a fork of block open1, this block will lose the election
	auto open2 = builder.make_block ()
				 .source (send1->hash ())
				 .representative (rep2.pub)
				 .account (key1.pub)
				 .sign (key1.prv, key1.pub)
				 .work (*system.work.generate (key1.pub))
				 .build ();
	ASSERT_FALSE (*open1 == *open2);

	// give block open1 to node1, manually trigger an election for open1 and ensure it is in the ledger
	node1.process_active (open1);
	ASSERT_TIMELY (5s, node1.block (open1->hash ()) != nullptr);
	node1.scheduler.manual.push (open1);
	ASSERT_TIMELY (5s, (election = node1.active.election (open1->qualified_root ())) != nullptr);
	election->transition_active ();

	// create node2, with blocks send1 and open2 pre-initialised in the ledger,
	// so that block open1 cannot possibly get in the ledger before open2 via background sync
	system.initialization_blocks.push_back (send1);
	system.initialization_blocks.push_back (open2);
	auto & node2 = *system.add_node ();
	system.initialization_blocks.clear ();

	// ensure open2 is in node2 ledger (and therefore has sideband) and manually trigger an election for open2
	ASSERT_TIMELY (5s, node2.block (open2->hash ()) != nullptr);
	node2.scheduler.manual.push (open2);
	ASSERT_TIMELY (5s, (election = node2.active.election (open2->qualified_root ())) != nullptr);
	election->transition_active ();

	ASSERT_TIMELY_EQ (5s, 2, node1.active.size ());
	ASSERT_TIMELY_EQ (5s, 2, node2.active.size ());

	// allow node1 to vote and wait for open1 to be confirmed on node1
	(void)node1.wallets.insert_adhoc (wallet_id, nano::dev::genesis_key.prv);
	ASSERT_TIMELY (5s, node1.block_confirmed (open1->hash ()));

	// Notify both nodes of both blocks, both nodes will become aware that a fork exists
	node1.process_active (open2);
	node2.process_active (open1);

	ASSERT_TIMELY_EQ (5s, 2, election->votes ().size ()); // one more than expected due to elections having dummy votes

	// Node2 should eventually settle on open1
	ASSERT_TIMELY (10s, node2.block (open1->hash ()));
	ASSERT_TIMELY (5s, node1.block_confirmed (open1->hash ()));
	auto winner = *node2.active.tally (*election).begin ();
	ASSERT_EQ (*open1, *winner.second);
	ASSERT_EQ (nano::dev::constants.genesis_amount - 1, winner.first);

	// check the correct blocks are in the ledgers
	auto transaction1 (node1.store.tx_begin_read ());
	auto transaction2 (node2.store.tx_begin_read ());
	ASSERT_TRUE (node1.ledger.any ().block_exists (*transaction1, open1->hash ()));
	ASSERT_TRUE (node2.ledger.any ().block_exists (*transaction2, open1->hash ()));
	ASSERT_FALSE (node2.ledger.any ().block_exists (*transaction2, open2->hash ()));
}

TEST (node, coherent_observer)
{
	nano::test::system system (1);
	auto & node1 (*system.nodes[0]);
	auto wallet_id = node1.wallets.first_wallet_id ();
	node1.observers->blocks.add ([&node1] (nano::election_status const & status_a, std::vector<nano::vote_with_weight_info> const &, nano::account const &, nano::uint128_t const &, bool, bool) {
		auto transaction (node1.store.tx_begin_read ());
		ASSERT_TRUE (node1.ledger.any ().block_exists (*transaction, status_a.get_winner ()->hash ()));
	});
	(void)node1.wallets.insert_adhoc (wallet_id, nano::dev::genesis_key.prv);
	nano::keypair key;
	node1.wallets.send_action (wallet_id, nano::dev::genesis_key.pub, key.pub, 1);
}

TEST (node, rep_self_vote)
{
	nano::test::system system;
	nano::node_config node_config (system.get_available_port ());
	node_config.online_weight_minimum = std::numeric_limits<nano::uint128_t>::max ();
	node_config.frontiers_confirmation = nano::frontiers_confirmation_mode::disabled;
	auto node0 = system.add_node (node_config);
	auto wallet_id = node0->wallets.first_wallet_id ();
	nano::keypair rep_big;
	nano::block_builder builder;
	auto fund_big = builder.send ()
					.previous (nano::dev::genesis->hash ())
					.destination (rep_big.pub)
					.balance (nano::uint128_t{ "0xb0000000000000000000000000000000" })
					.sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
					.work (*system.work.generate (nano::dev::genesis->hash ()))
					.build ();
	auto open_big = builder.open ()
					.source (fund_big->hash ())
					.representative (rep_big.pub)
					.account (rep_big.pub)
					.sign (rep_big.prv, rep_big.pub)
					.work (*system.work.generate (rep_big.pub))
					.build ();
	ASSERT_EQ (nano::block_status::progress, node0->process (fund_big));
	ASSERT_EQ (nano::block_status::progress, node0->process (open_big));
	// Confirm both blocks, allowing voting on the upcoming block
	node0->start_election (node0->block (open_big->hash ()));
	std::shared_ptr<nano::election> election;
	ASSERT_TIMELY (5s, election = node0->active.election (open_big->qualified_root ()));
	node0->active.force_confirm (*election);

	(void)node0->wallets.insert_adhoc (wallet_id, rep_big.prv);
	(void)node0->wallets.insert_adhoc (wallet_id, nano::dev::genesis_key.prv);
	ASSERT_EQ (node0->wallets.voting_reps_count (), 2);
	auto block0 = builder.send ()
				  .previous (fund_big->hash ())
				  .destination (rep_big.pub)
				  .balance (nano::uint128_t ("0x60000000000000000000000000000000"))
				  .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				  .work (*system.work.generate (fund_big->hash ()))
				  .build ();
	ASSERT_EQ (nano::block_status::progress, node0->process (block0));
	auto & active = node0->active;
	auto & scheduler = node0->scheduler;
	auto election1 = nano::test::start_election (system, *node0, block0->hash ());
	ASSERT_NE (nullptr, election1);
	// Wait until representatives are activated & make vote
	ASSERT_TIMELY_EQ (1s, election1->votes ().size (), 3);
	auto rep_votes (election1->votes ());
	ASSERT_NE (rep_votes.end (), rep_votes.find (nano::dev::genesis_key.pub));
	ASSERT_NE (rep_votes.end (), rep_votes.find (rep_big.pub));
}

// Bootstrapping a forked open block should succeed.
TEST (node, bootstrap_fork_open)
{
	nano::test::system system;
	nano::node_config node_config (system.get_available_port ());
	auto node0 = system.add_node (node_config);
	auto wallet_id0 = node0->wallets.first_wallet_id ();
	node_config.peering_port = system.get_available_port ();
	auto node1 = system.add_node (node_config);
	nano::keypair key0;
	nano::block_builder builder;
	auto send0 = builder.send ()
				 .previous (nano::dev::genesis->hash ())
				 .destination (key0.pub)
				 .balance (nano::dev::constants.genesis_amount - 500)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (nano::dev::genesis->hash ()))
				 .build ();
	auto open0 = builder.open ()
				 .source (send0->hash ())
				 .representative (1)
				 .account (key0.pub)
				 .sign (key0.prv, key0.pub)
				 .work (*system.work.generate (key0.pub))
				 .build ();
	auto open1 = builder.open ()
				 .source (send0->hash ())
				 .representative (2)
				 .account (key0.pub)
				 .sign (key0.prv, key0.pub)
				 .work (*system.work.generate (key0.pub))
				 .build ();
	// Both know about send0
	ASSERT_EQ (nano::block_status::progress, node0->process (send0));
	ASSERT_EQ (nano::block_status::progress, node1->process (send0));
	// Confirm send0 to allow starting and voting on the following blocks
	for (auto node : system.nodes)
	{
		node->start_election (node->block (node->latest (nano::dev::genesis_key.pub)));
		ASSERT_TIMELY (1s, node->active.election (send0->qualified_root ()));
		auto election = node->active.election (send0->qualified_root ());
		ASSERT_NE (nullptr, election);
		node->active.force_confirm (*election);
		ASSERT_TIMELY (2s, node->active.empty ());
	}
	ASSERT_TIMELY (3s, node0->block_confirmed (send0->hash ()));
	// They disagree about open0/open1
	ASSERT_EQ (nano::block_status::progress, node0->process (open0));
	ASSERT_EQ (nano::block_status::progress, node1->process (open1));
	(void)node0->wallets.insert_adhoc (wallet_id0, nano::dev::genesis_key.prv);
	ASSERT_FALSE (node1->block_or_pruned_exists (open0->hash ()));
	ASSERT_FALSE (node1->bootstrap_initiator.in_progress ());
	node1->bootstrap_initiator.bootstrap (node0->network->endpoint ());
	ASSERT_TIMELY (1s, node1->active.empty ());
	ASSERT_TIMELY (10s, !node1->block_or_pruned_exists (open1->hash ()) && node1->block_or_pruned_exists (open0->hash ()));
}

// Unconfirmed blocks from bootstrap should be confirmed
TEST (node, bootstrap_confirm_frontiers)
{
	// create 2 separate systems, the 2 system do not interact with each other automatically
	nano::test::system system0 (1);
	nano::test::system system1 (1);
	auto node0 = system0.nodes[0];
	auto node1 = system1.nodes[0];
	auto wallet_id0 = node0->wallets.first_wallet_id ();
	auto wallet_id1 = node1->wallets.first_wallet_id ();
	(void)node0->wallets.insert_adhoc (wallet_id0, nano::dev::genesis_key.prv);
	nano::keypair key0;

	// create block to send 500 raw from genesis to key0 and save into node0 ledger without immediately triggering an election
	auto send0 = nano::send_block_builder ()
				 .previous (nano::dev::genesis->hash ())
				 .destination (key0.pub)
				 .balance (nano::dev::constants.genesis_amount - 500)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*node0->work_generate_blocking (nano::dev::genesis->hash ()))
				 .build ();
	ASSERT_EQ (nano::block_status::progress, node0->process (send0));

	// each system only has one node, so there should be no bootstrapping going on
	ASSERT_FALSE (node0->bootstrap_initiator.in_progress ());
	ASSERT_FALSE (node1->bootstrap_initiator.in_progress ());
	ASSERT_TRUE (node1->active.empty ());

	// create a bootstrap connection from node1 to node0
	// this also has the side effect of adding node0 to node1's list of peers, which will trigger realtime connections too
	node1->connect (node0->network->endpoint ());
	node1->bootstrap_initiator.bootstrap (node0->network->endpoint ());

	// Wait until the block is confirmed on node1. Poll more than usual because we are polling
	// on 2 different systems at once and in sequence and there might be strange timing effects.
	system0.deadline_set (10s);
	system1.deadline_set (10s);
	while (true)
	{
		{
			auto tx{ node1->store.tx_begin_read () };
			if (node1->ledger.confirmed ().block_exists (*tx, send0->hash ()))
			{
				break;
			}
		}
		ASSERT_NO_ERROR (system0.poll (std::chrono::milliseconds (1)));
		ASSERT_NO_ERROR (system1.poll (std::chrono::milliseconds (1)));
	}
}

// Test that if we create a block that isn't confirmed, the bootstrapping processes sync the missing block.
TEST (node, unconfirmed_send)
{
	nano::test::system system{};

	auto & node1 = *system.add_node ();
	auto wallet_id1 = node1.wallets.first_wallet_id ();
	(void)node1.wallets.insert_adhoc (wallet_id1, nano::dev::genesis_key.prv);

	nano::keypair key2{};
	auto & node2 = *system.add_node ();
	auto wallet_id2 = node2.wallets.first_wallet_id ();
	(void)node2.wallets.insert_adhoc (wallet_id2, key2.prv);

	// firstly, send two units from node1 to node2 and expect that both nodes see the block as confirmed
	// (node1 will start an election for it, vote on it and node2 gets synced up)
	auto send1 = node1.wallets.send_action (wallet_id1, nano::dev::genesis_key.pub, key2.pub, 2 * nano::Mxrb_ratio);
	ASSERT_TIMELY (5s, node1.block_confirmed (send1->hash ()));
	ASSERT_TIMELY (5s, node2.block_confirmed (send1->hash ()));

	// wait until receive1 (auto-receive created by wallet) is cemented
	ASSERT_TIMELY_EQ (5s, node2.get_confirmation_height (*node2.store.tx_begin_read (), key2.pub), 1);
	ASSERT_EQ (node2.balance (key2.pub), 2 * nano::Mxrb_ratio);
	auto recv1 = node2.ledger.find_receive_block_by_send_hash (*node2.store.tx_begin_read (), key2.pub, send1->hash ());

	// create send2 to send from node2 to node1 and save it to node2's ledger without triggering an election (node1 does not hear about it)
	auto send2 = nano::state_block_builder{}
				 .make_block ()
				 .account (key2.pub)
				 .previous (recv1->hash ())
				 .representative (nano::dev::genesis_key.pub)
				 .balance (nano::Mxrb_ratio)
				 .link (nano::dev::genesis_key.pub)
				 .sign (key2.prv, key2.pub)
				 .work (*system.work.generate (recv1->hash ()))
				 .build ();
	ASSERT_EQ (nano::block_status::progress, node2.process (send2));

	auto send3 = node2.wallets.send_action (wallet_id2, key2.pub, nano::dev::genesis_key.pub, nano::Mxrb_ratio);
	ASSERT_TIMELY (5s, node2.block_confirmed (send2->hash ()));
	ASSERT_TIMELY (5s, node1.block_confirmed (send2->hash ()));
	ASSERT_TIMELY (5s, node2.block_confirmed (send3->hash ()));
	ASSERT_TIMELY (5s, node1.block_confirmed (send3->hash ()));
	ASSERT_TIMELY_EQ (5s, node2.ledger.cemented_count (), 7);
	ASSERT_TIMELY_EQ (5s, node1.balance (nano::dev::genesis_key.pub), nano::dev::constants.genesis_amount);
}

// Test that nodes can disable representative voting
TEST (node, no_voting)
{
	nano::test::system system (1);
	auto & node0 (*system.nodes[0]);
	nano::node_config node_config (system.get_available_port ());
	node_config.enable_voting = false;
	auto node1 = system.add_node (node_config);

	auto wallet_id0 = node0.wallets.first_wallet_id ();
	auto wallet_id1 = node1->wallets.first_wallet_id ();
	// Node1 has a rep
	(void)node1->wallets.insert_adhoc (wallet_id1, nano::dev::genesis_key.prv);
	nano::keypair key1;
	(void)node1->wallets.insert_adhoc (wallet_id1, key1.prv);
	// Broadcast a confirm so others should know this is a rep node
	node1->wallets.send_action (wallet_id1, nano::dev::genesis_key.pub, key1.pub, nano::Mxrb_ratio);
	ASSERT_TIMELY (10s, node0.active.empty ());
	ASSERT_EQ (0, node0.stats->count (nano::stat::type::message, nano::stat::detail::confirm_ack, nano::stat::dir::in));
}

TEST (node, send_callback)
{
	nano::test::system system (1);
	auto & node0 (*system.nodes[0]);
	auto wallet_id = node0.wallets.first_wallet_id ();
	nano::keypair key2;
	(void)node0.wallets.insert_adhoc (wallet_id, nano::dev::genesis_key.prv);
	(void)node0.wallets.insert_adhoc (wallet_id, key2.prv);
	node0.config->callback_address = "localhost";
	node0.config->callback_port = 8010;
	node0.config->callback_target = "/";
	ASSERT_NE (nullptr, node0.wallets.send_action (wallet_id, nano::dev::genesis_key.pub, key2.pub, node0.config->receive_minimum.number ()));
	ASSERT_TIMELY (10s, node0.balance (key2.pub).is_zero ());
	ASSERT_EQ (std::numeric_limits<nano::uint128_t>::max () - node0.config->receive_minimum.number (), node0.balance (nano::dev::genesis_key.pub));
}

TEST (node, balance_observer)
{
	nano::test::system system (1);
	auto & node1 (*system.nodes[0]);
	auto wallet_id = node1.wallets.first_wallet_id ();
	std::atomic<int> balances (0);
	nano::keypair key;
	node1.observers->account_balance.add ([&key, &balances] (nano::account const & account_a, bool is_pending) {
		if (key.pub == account_a && is_pending)
		{
			balances++;
		}
		else if (nano::dev::genesis_key.pub == account_a && !is_pending)
		{
			balances++;
		}
	});
	(void)node1.wallets.insert_adhoc (wallet_id, nano::dev::genesis_key.prv);
	node1.wallets.send_action (wallet_id, nano::dev::genesis_key.pub, key.pub, 1);
	system.deadline_set (10s);
	auto done (false);
	while (!done)
	{
		auto ec = system.poll ();
		done = balances.load () == 2;
		ASSERT_NO_ERROR (ec);
	}
}

TEST (node, bootstrap_connection_scaling)
{
	nano::test::system system (1);
	auto & node1 (*system.nodes[0]);
	ASSERT_EQ (34, node1.bootstrap_initiator.connections->target_connections (5000, 1));
	ASSERT_EQ (4, node1.bootstrap_initiator.connections->target_connections (0, 1));
	ASSERT_EQ (64, node1.bootstrap_initiator.connections->target_connections (50000, 1));
	ASSERT_EQ (64, node1.bootstrap_initiator.connections->target_connections (10000000000, 1));
	ASSERT_EQ (32, node1.bootstrap_initiator.connections->target_connections (5000, 0));
	ASSERT_EQ (1, node1.bootstrap_initiator.connections->target_connections (0, 0));
	ASSERT_EQ (64, node1.bootstrap_initiator.connections->target_connections (50000, 0));
	ASSERT_EQ (64, node1.bootstrap_initiator.connections->target_connections (10000000000, 0));
	ASSERT_EQ (36, node1.bootstrap_initiator.connections->target_connections (5000, 2));
	ASSERT_EQ (8, node1.bootstrap_initiator.connections->target_connections (0, 2));
	ASSERT_EQ (64, node1.bootstrap_initiator.connections->target_connections (50000, 2));
	ASSERT_EQ (64, node1.bootstrap_initiator.connections->target_connections (10000000000, 2));
	// TODO: config changes after node started are not supported!
	// node1.config->bootstrap_connections = 128;
	// ASSERT_EQ (64, node1.bootstrap_initiator.connections->target_connections (0, 1));
	// ASSERT_EQ (64, node1.bootstrap_initiator.connections->target_connections (50000, 1));
	// ASSERT_EQ (64, node1.bootstrap_initiator.connections->target_connections (0, 2));
	// ASSERT_EQ (64, node1.bootstrap_initiator.connections->target_connections (50000, 2));
	// node1.config->bootstrap_connections_max = 256;
	// ASSERT_EQ (128, node1.bootstrap_initiator.connections->target_connections (0, 1));
	// ASSERT_EQ (256, node1.bootstrap_initiator.connections->target_connections (50000, 1));
	// ASSERT_EQ (256, node1.bootstrap_initiator.connections->target_connections (0, 2));
	// ASSERT_EQ (256, node1.bootstrap_initiator.connections->target_connections (50000, 2));
	// node1.config->bootstrap_connections_max = 0;
	// ASSERT_EQ (1, node1.bootstrap_initiator.connections->target_connections (0, 1));
	// ASSERT_EQ (1, node1.bootstrap_initiator.connections->target_connections (50000, 1));
}

TEST (node, block_confirm)
{
	auto type = nano::transport::transport_type::tcp;
	nano::node_flags node_flags;
	nano::test::system system (2, type, node_flags);
	auto & node1 (*system.nodes[0]);
	auto & node2 (*system.nodes[1]);
	auto wallet_id1 = node1.wallets.first_wallet_id ();
	auto wallet_id2 = node2.wallets.first_wallet_id ();
	nano::keypair key;
	nano::state_block_builder builder;
	auto send1 = builder.make_block ()
				 .account (nano::dev::genesis_key.pub)
				 .previous (nano::dev::genesis->hash ())
				 .representative (nano::dev::genesis_key.pub)
				 .balance (nano::dev::constants.genesis_amount - nano::Gxrb_ratio)
				 .link (key.pub)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*node1.work_generate_blocking (nano::dev::genesis->hash ()))
				 .build ();
	// A copy is necessary to avoid data races during ledger processing, which sets the sideband
	auto send1_copy = builder.make_block ()
					  .from (*send1)
					  .build ();
	auto hash1 = send1->hash ();
	auto hash2 = send1_copy->hash ();
	node1.block_processor.add (send1);
	node2.block_processor.add (send1_copy);
	ASSERT_TIMELY (5s, node1.block_or_pruned_exists (send1->hash ()) && node2.block_or_pruned_exists (send1_copy->hash ()));
	ASSERT_TRUE (node1.block_or_pruned_exists (send1->hash ()));
	ASSERT_TRUE (node2.block_or_pruned_exists (send1_copy->hash ()));
	// Confirm send1 on node2 so it can vote for send2
	node2.start_election (send1_copy);
	std::shared_ptr<nano::election> election;
	ASSERT_TIMELY (5s, election = node2.active.election (send1_copy->qualified_root ()));
	// Make node2 genesis representative so it can vote
	(void)node2.wallets.insert_adhoc (wallet_id2, nano::dev::genesis_key.prv);
	ASSERT_TIMELY_EQ (10s, node1.active.recently_cemented_size (), 1);
}

TEST (node, confirm_quorum)
{
	nano::test::system system (1);
	auto & node1 = *system.nodes[0];
	auto wallet_id = node1.wallets.first_wallet_id ();
	(void)node1.wallets.insert_adhoc (wallet_id, nano::dev::genesis_key.prv);
	// Put greater than node.delta () in pending so quorum can't be reached
	nano::amount new_balance = node1.quorum ().quorum_delta.number () - nano::Gxrb_ratio;
	auto send1 = nano::state_block_builder ()
				 .account (nano::dev::genesis_key.pub)
				 .previous (nano::dev::genesis->hash ())
				 .representative (nano::dev::genesis_key.pub)
				 .balance (new_balance)
				 .link (nano::dev::genesis_key.pub)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*node1.work_generate_blocking (nano::dev::genesis->hash ()))
				 .build ();
	ASSERT_EQ (nano::block_status::progress, node1.process (send1));
	node1.wallets.send_action (wallet_id, nano::dev::genesis_key.pub, nano::dev::genesis_key.pub, new_balance.number ());
	ASSERT_TIMELY (2s, node1.active.election (send1->qualified_root ()));
	auto election = node1.active.election (send1->qualified_root ());
	ASSERT_NE (nullptr, election);
	ASSERT_FALSE (node1.active.confirmed (*election));
	ASSERT_EQ (1, election->votes ().size ());
	ASSERT_EQ (0, node1.balance (nano::dev::genesis_key.pub));
}

TEST (node, vote_by_hash_bundle)
{
	// Keep max_hashes above system to ensure it is kept in scope as votes can be added during system destruction
	std::atomic<size_t> max_hashes{ 0 };
	nano::test::system system (1);
	auto & node = *system.nodes[0];
	auto wallet_id = node.wallets.first_wallet_id ();
	nano::state_block_builder builder;
	std::vector<std::shared_ptr<nano::state_block>> blocks;
	auto block = builder.make_block ()
				 .account (nano::dev::genesis_key.pub)
				 .previous (nano::dev::genesis->hash ())
				 .representative (nano::dev::genesis_key.pub)
				 .balance (nano::dev::constants.genesis_amount - 1)
				 .link (nano::dev::genesis_key.pub)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (nano::dev::genesis->hash ()))
				 .build ();
	blocks.push_back (block);
	ASSERT_EQ (nano::block_status::progress, node.ledger.process (*node.store.tx_begin_write (), blocks.back ()));
	for (auto i = 2; i < 200; ++i)
	{
		auto block = builder.make_block ()
					 .from (*blocks.back ())
					 .previous (blocks.back ()->hash ())
					 .balance (nano::dev::constants.genesis_amount - i)
					 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
					 .work (*system.work.generate (blocks.back ()->hash ()))
					 .build ();
		blocks.push_back (block);
		ASSERT_EQ (nano::block_status::progress, node.ledger.process (*node.store.tx_begin_write (), blocks.back ()));
	}

	// Confirming last block will confirm whole chain and allow us to generate votes for those blocks later
	nano::test::confirm (node.ledger, blocks.back ());

	(void)node.wallets.insert_adhoc (wallet_id, nano::dev::genesis_key.prv);
	nano::keypair key1;
	(void)node.wallets.insert_adhoc (wallet_id, key1.prv);

	system.nodes[0]->observers->vote.add ([&max_hashes] (std::shared_ptr<nano::vote> const & vote_a, nano::vote_source, nano::vote_code) {
		if (vote_a->hashes ().size () > max_hashes)
		{
			max_hashes = vote_a->hashes ().size ();
		}
	});

	for (auto const & block : blocks)
	{
		system.nodes[0]->enqueue_vote_request (block->root (), block->hash ());
	}

	// Verify that bundling occurs. While reaching 12 should be common on most hardware in release mode,
	// we set this low enough to allow the test to pass on CI/with sanitizers.
	ASSERT_TIMELY (20s, max_hashes.load () >= 3);
}

TEST (node, block_processor_signatures)
{
	nano::test::system system{ 1 };
	auto & node1 = *system.nodes[0];
	(void)node1.wallets.insert_adhoc (node1.wallets.first_wallet_id (), nano::dev::genesis_key.prv);
	nano::block_hash latest = system.nodes[0]->latest (nano::dev::genesis_key.pub);
	nano::state_block_builder builder;
	nano::keypair key1;
	nano::keypair key2;
	nano::keypair key3;
	auto send1 = builder.make_block ()
				 .account (nano::dev::genesis_key.pub)
				 .previous (latest)
				 .representative (nano::dev::genesis_key.pub)
				 .balance (nano::dev::constants.genesis_amount - nano::Gxrb_ratio)
				 .link (key1.pub)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*node1.work_generate_blocking (latest))
				 .build ();
	auto send2 = builder.make_block ()
				 .account (nano::dev::genesis_key.pub)
				 .previous (send1->hash ())
				 .representative (nano::dev::genesis_key.pub)
				 .balance (nano::dev::constants.genesis_amount - 2 * nano::Gxrb_ratio)
				 .link (key2.pub)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*node1.work_generate_blocking (send1->hash ()))
				 .build ();
	auto send3 = builder.make_block ()
				 .account (nano::dev::genesis_key.pub)
				 .previous (send2->hash ())
				 .representative (nano::dev::genesis_key.pub)
				 .balance (nano::dev::constants.genesis_amount - 3 * nano::Gxrb_ratio)
				 .link (key3.pub)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*node1.work_generate_blocking (send2->hash ()))
				 .build ();
	// Invalid signature bit
	auto send4 = builder.make_block ()
				 .account (nano::dev::genesis_key.pub)
				 .previous (send3->hash ())
				 .representative (nano::dev::genesis_key.pub)
				 .balance (nano::dev::constants.genesis_amount - 4 * nano::Gxrb_ratio)
				 .link (key3.pub)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*node1.work_generate_blocking (send3->hash ()))
				 .build ();
	auto sig{ send4->block_signature () };
	sig.bytes[32] ^= 0x1;
	send4->signature_set (sig);
	// Invalid signature bit (force)
	auto send5 = builder.make_block ()
				 .account (nano::dev::genesis_key.pub)
				 .previous (send3->hash ())
				 .representative (nano::dev::genesis_key.pub)
				 .balance (nano::dev::constants.genesis_amount - 5 * nano::Gxrb_ratio)
				 .link (key3.pub)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*node1.work_generate_blocking (send3->hash ()))
				 .build ();
	auto signature = send5->block_signature ();
	signature.bytes[31] ^= 0x1;
	send5->signature_set (signature);
	// Invalid signature to unchecked
	node1.unchecked.put (send5->previous (), nano::unchecked_info{ send5 });
	auto receive1 = builder.make_block ()
					.account (key1.pub)
					.previous (0)
					.representative (nano::dev::genesis_key.pub)
					.balance (nano::Gxrb_ratio)
					.link (send1->hash ())
					.sign (key1.prv, key1.pub)
					.work (*node1.work_generate_blocking (key1.pub))
					.build ();
	auto receive2 = builder.make_block ()
					.account (key2.pub)
					.previous (0)
					.representative (nano::dev::genesis_key.pub)
					.balance (nano::Gxrb_ratio)
					.link (send2->hash ())
					.sign (key2.prv, key2.pub)
					.work (*node1.work_generate_blocking (key2.pub))
					.build ();
	// Invalid private key
	auto receive3 = builder.make_block ()
					.account (key3.pub)
					.previous (0)
					.representative (nano::dev::genesis_key.pub)
					.balance (nano::Gxrb_ratio)
					.link (send3->hash ())
					.sign (key2.prv, key3.pub)
					.work (*node1.work_generate_blocking (key3.pub))
					.build ();
	node1.process_active (send1);
	node1.process_active (send2);
	node1.process_active (send3);
	node1.process_active (send4);
	node1.process_active (receive1);
	node1.process_active (receive2);
	node1.process_active (receive3);
	ASSERT_TIMELY (5s, node1.block (receive2->hash ()) != nullptr); // Implies send1, send2, send3, receive1.
	ASSERT_TIMELY_EQ (5s, node1.unchecked.count (), 0);
	ASSERT_EQ (nullptr, node1.block (receive3->hash ())); // Invalid signer
	ASSERT_EQ (nullptr, node1.block (send4->hash ())); // Invalid signature via process_active
	ASSERT_EQ (nullptr, node1.block (send5->hash ())); // Invalid signature via unchecked
}

/*
 *  State blocks go through a different signature path, ensure invalidly signed state blocks are rejected
 *  This test can freeze if the wake conditions in block_processor::flush are off, for that reason this is done async here
 */
TEST (node, block_processor_reject_state)
{
	nano::test::system system (1);
	auto & node (*system.nodes[0]);
	nano::state_block_builder builder;
	auto send1 = builder.make_block ()
				 .account (nano::dev::genesis_key.pub)
				 .previous (nano::dev::genesis->hash ())
				 .representative (nano::dev::genesis_key.pub)
				 .balance (nano::dev::constants.genesis_amount - nano::Gxrb_ratio)
				 .link (nano::dev::genesis_key.pub)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*node.work_generate_blocking (nano::dev::genesis->hash ()))
				 .build ();
	auto sig{ send1->block_signature () };
	sig.bytes[0] ^= 1;
	send1->signature_set (sig);
	ASSERT_FALSE (node.block_or_pruned_exists (send1->hash ()));
	node.process_active (send1);
	ASSERT_TIMELY_EQ (5s, 1, node.stats->count (nano::stat::type::blockprocessor_result, nano::stat::detail::bad_signature));
	ASSERT_FALSE (node.block_or_pruned_exists (send1->hash ()));
	auto send2 = builder.make_block ()
				 .account (nano::dev::genesis_key.pub)
				 .previous (nano::dev::genesis->hash ())
				 .representative (nano::dev::genesis_key.pub)
				 .balance (nano::dev::constants.genesis_amount - 2 * nano::Gxrb_ratio)
				 .link (nano::dev::genesis_key.pub)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*node.work_generate_blocking (nano::dev::genesis->hash ()))
				 .build ();
	node.process_active (send2);
	ASSERT_TIMELY (5s, node.block_or_pruned_exists (send2->hash ()));
}

/** This checks that a node can be opened (without being blocked) when a write lock is held elsewhere */
TEST (node, dont_write_lock_node)
{
	auto path = nano::unique_path ();

	std::promise<void> write_lock_held_promise;
	std::promise<void> finished_promise;
	std::thread ([&path, &write_lock_held_promise, &finished_promise] () {
		auto store = nano::make_store (path, nano::dev::constants, false, true);

		// Hold write lock open until main thread is done needing it
		auto transaction (store->tx_begin_write ());
		write_lock_held_promise.set_value ();
		finished_promise.get_future ().wait ();
	})
	.detach ();

	write_lock_held_promise.get_future ().wait ();

	// Check inactive node can finish executing while a write lock is open
	nano::node_flags flags{ nano::inactive_node_flag_defaults () };
	nano::inactive_node node (path, flags);
	finished_promise.set_value ();
}

TEST (node, node_sequence)
{
	nano::test::system system (3);
	ASSERT_EQ (0, system.nodes[0]->node_seq);
	ASSERT_EQ (0, system.nodes[0]->node_seq);
	ASSERT_EQ (1, system.nodes[1]->node_seq);
	ASSERT_EQ (2, system.nodes[2]->node_seq);
}

TEST (node, rollback_gap_source)
{
	nano::test::system system;
	nano::node_config node_config (system.get_available_port ());
	node_config.frontiers_confirmation = nano::frontiers_confirmation_mode::disabled;
	auto & node = *system.add_node (node_config);
	nano::state_block_builder builder;
	nano::keypair key;
	auto send1 = builder.make_block ()
				 .account (nano::dev::genesis_key.pub)
				 .previous (nano::dev::genesis->hash ())
				 .representative (nano::dev::genesis_key.pub)
				 .link (key.pub)
				 .balance (nano::dev::constants.genesis_amount - 1)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (nano::dev::genesis->hash ()))
				 .build ();
	// Side a of a forked open block receiving from send1
	// This is a losing block
	auto fork1a = builder.make_block ()
				  .account (key.pub)
				  .previous (0)
				  .representative (key.pub)
				  .link (send1->hash ())
				  .balance (1)
				  .sign (key.prv, key.pub)
				  .work (*system.work.generate (key.pub))
				  .build ();
	auto send2 = builder.make_block ()
				 .from (*send1)
				 .previous (send1->hash ())
				 .balance (send1->balance_field ().value ().number () - 1)
				 .link (key.pub)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (send1->hash ()))
				 .build ();
	// Side b of a forked open block receiving from send2.
	// This is the winning block
	auto fork1b = builder.make_block ()
				  .from (*fork1a)
				  .link (send2->hash ())
				  .sign (key.prv, key.pub)
				  .build ();
	// Set 'node' up with losing block 'fork1a'
	ASSERT_EQ (nano::block_status::progress, node.process (send1));
	ASSERT_EQ (nano::block_status::progress, node.process (fork1a));
	// Node has 'fork1a' & doesn't have source 'send2' for winning 'fork1b' block
	ASSERT_EQ (nullptr, node.block (send2->hash ()));
	node.block_processor.force (fork1b);
	ASSERT_TIMELY_EQ (5s, node.block (fork1a->hash ()), nullptr);
	// Wait for the rollback (attempt to replace fork with open)
	ASSERT_TIMELY_EQ (5s, node.stats->count (nano::stat::type::rollback, nano::stat::detail::open), 1);
	// But replacing is not possible (missing source block - send2)
	ASSERT_EQ (nullptr, node.block (fork1b->hash ()));
	// Fork can be returned by some other forked node
	node.process_active (fork1a);
	ASSERT_TIMELY (5s, node.block (fork1a->hash ()) != nullptr);
	// With send2 block in ledger election can start again to remove fork block
	ASSERT_EQ (nano::block_status::progress, node.process (send2));
	node.block_processor.force (fork1b);
	// Wait for new rollback
	ASSERT_TIMELY_EQ (5s, node.stats->count (nano::stat::type::rollback, nano::stat::detail::open), 2);
	// Now fork block should be replaced with open
	ASSERT_TIMELY (5s, node.block (fork1b->hash ()) != nullptr);
	ASSERT_EQ (nullptr, node.block (fork1a->hash ()));
}

// Confirm a complex dependency graph starting from the first block
TEST (node, dependency_graph)
{
	nano::test::system system;
	nano::node_config config (system.get_available_port ());
	config.frontiers_confirmation = nano::frontiers_confirmation_mode::disabled;
	auto & node = *system.add_node (config);
	auto wallet_id = node.wallets.first_wallet_id ();

	nano::state_block_builder builder;
	nano::keypair key1, key2, key3;

	// Send to key1
	auto gen_send1 = builder.make_block ()
					 .account (nano::dev::genesis_key.pub)
					 .previous (nano::dev::genesis->hash ())
					 .representative (nano::dev::genesis_key.pub)
					 .link (key1.pub)
					 .balance (nano::dev::constants.genesis_amount - 1)
					 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
					 .work (*system.work.generate (nano::dev::genesis->hash ()))
					 .build ();
	// Receive from genesis
	auto key1_open = builder.make_block ()
					 .account (key1.pub)
					 .previous (0)
					 .representative (key1.pub)
					 .link (gen_send1->hash ())
					 .balance (1)
					 .sign (key1.prv, key1.pub)
					 .work (*system.work.generate (key1.pub))
					 .build ();
	// Send to genesis
	auto key1_send1 = builder.make_block ()
					  .account (key1.pub)
					  .previous (key1_open->hash ())
					  .representative (key1.pub)
					  .link (nano::dev::genesis_key.pub)
					  .balance (0)
					  .sign (key1.prv, key1.pub)
					  .work (*system.work.generate (key1_open->hash ()))
					  .build ();
	// Receive from key1
	auto gen_receive = builder.make_block ()
					   .from (*gen_send1)
					   .previous (gen_send1->hash ())
					   .link (key1_send1->hash ())
					   .balance (nano::dev::constants.genesis_amount)
					   .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
					   .work (*system.work.generate (gen_send1->hash ()))
					   .build ();
	// Send to key2
	auto gen_send2 = builder.make_block ()
					 .from (*gen_receive)
					 .previous (gen_receive->hash ())
					 .link (key2.pub)
					 .balance (gen_receive->balance_field ().value ().number () - 2)
					 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
					 .work (*system.work.generate (gen_receive->hash ()))
					 .build ();
	// Receive from genesis
	auto key2_open = builder.make_block ()
					 .account (key2.pub)
					 .previous (0)
					 .representative (key2.pub)
					 .link (gen_send2->hash ())
					 .balance (2)
					 .sign (key2.prv, key2.pub)
					 .work (*system.work.generate (key2.pub))
					 .build ();
	// Send to key3
	auto key2_send1 = builder.make_block ()
					  .account (key2.pub)
					  .previous (key2_open->hash ())
					  .representative (key2.pub)
					  .link (key3.pub)
					  .balance (1)
					  .sign (key2.prv, key2.pub)
					  .work (*system.work.generate (key2_open->hash ()))
					  .build ();
	// Receive from key2
	auto key3_open = builder.make_block ()
					 .account (key3.pub)
					 .previous (0)
					 .representative (key3.pub)
					 .link (key2_send1->hash ())
					 .balance (1)
					 .sign (key3.prv, key3.pub)
					 .work (*system.work.generate (key3.pub))
					 .build ();
	// Send to key1
	auto key2_send2 = builder.make_block ()
					  .from (*key2_send1)
					  .previous (key2_send1->hash ())
					  .link (key1.pub)
					  .balance (key2_send1->balance_field ().value ().number () - 1)
					  .sign (key2.prv, key2.pub)
					  .work (*system.work.generate (key2_send1->hash ()))
					  .build ();
	// Receive from key2
	auto key1_receive = builder.make_block ()
						.from (*key1_send1)
						.previous (key1_send1->hash ())
						.link (key2_send2->hash ())
						.balance (key1_send1->balance_field ().value ().number () + 1)
						.sign (key1.prv, key1.pub)
						.work (*system.work.generate (key1_send1->hash ()))
						.build ();
	// Send to key3
	auto key1_send2 = builder.make_block ()
					  .from (*key1_receive)
					  .previous (key1_receive->hash ())
					  .link (key3.pub)
					  .balance (key1_receive->balance_field ().value ().number () - 1)
					  .sign (key1.prv, key1.pub)
					  .work (*system.work.generate (key1_receive->hash ()))
					  .build ();
	// Receive from key1
	auto key3_receive = builder.make_block ()
						.from (*key3_open)
						.previous (key3_open->hash ())
						.link (key1_send2->hash ())
						.balance (key3_open->balance_field ().value ().number () + 1)
						.sign (key3.prv, key3.pub)
						.work (*system.work.generate (key3_open->hash ()))
						.build ();
	// Upgrade key3
	auto key3_epoch = builder.make_block ()
					  .from (*key3_receive)
					  .previous (key3_receive->hash ())
					  .link (node.ledger.epoch_link (nano::epoch::epoch_1))
					  .balance (key3_receive->balance_field ().value ())
					  .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
					  .work (*system.work.generate (key3_receive->hash ()))
					  .build ();

	ASSERT_EQ (nano::block_status::progress, node.process (gen_send1));
	ASSERT_EQ (nano::block_status::progress, node.process (key1_open));
	ASSERT_EQ (nano::block_status::progress, node.process (key1_send1));
	ASSERT_EQ (nano::block_status::progress, node.process (gen_receive));
	ASSERT_EQ (nano::block_status::progress, node.process (gen_send2));
	ASSERT_EQ (nano::block_status::progress, node.process (key2_open));
	ASSERT_EQ (nano::block_status::progress, node.process (key2_send1));
	ASSERT_EQ (nano::block_status::progress, node.process (key3_open));
	ASSERT_EQ (nano::block_status::progress, node.process (key2_send2));
	ASSERT_EQ (nano::block_status::progress, node.process (key1_receive));
	ASSERT_EQ (nano::block_status::progress, node.process (key1_send2));
	ASSERT_EQ (nano::block_status::progress, node.process (key3_receive));
	ASSERT_EQ (nano::block_status::progress, node.process (key3_epoch));
	ASSERT_TRUE (node.active.empty ());

	// Hash -> Ancestors
	std::unordered_map<nano::block_hash, std::vector<nano::block_hash>> dependency_graph{
		{ key1_open->hash (), { gen_send1->hash () } },
		{ key1_send1->hash (), { key1_open->hash () } },
		{ gen_receive->hash (), { gen_send1->hash (), key1_open->hash () } },
		{ gen_send2->hash (), { gen_receive->hash () } },
		{ key2_open->hash (), { gen_send2->hash () } },
		{ key2_send1->hash (), { key2_open->hash () } },
		{ key3_open->hash (), { key2_send1->hash () } },
		{ key2_send2->hash (), { key2_send1->hash () } },
		{ key1_receive->hash (), { key1_send1->hash (), key2_send2->hash () } },
		{ key1_send2->hash (), { key1_send1->hash () } },
		{ key3_receive->hash (), { key3_open->hash (), key1_send2->hash () } },
		{ key3_epoch->hash (), { key3_receive->hash () } },
	};
	ASSERT_EQ (node.ledger.block_count () - 2, dependency_graph.size ());

	// Start an election for the first block of the dependency graph, and ensure all blocks are eventually confirmed
	(void)node.wallets.insert_adhoc (wallet_id, nano::dev::genesis_key.prv);
	node.start_election (gen_send1);

	ASSERT_NO_ERROR (system.poll_until_true (15s, [&] {
		// Not many blocks should be active simultaneously
		EXPECT_LT (node.active.size (), 6);

		// Ensure that active blocks have their ancestors confirmed
		auto error = std::any_of (dependency_graph.cbegin (), dependency_graph.cend (), [&] (auto entry) {
			if (node.election_active (entry.first))
			{
				for (auto ancestor : entry.second)
				{
					if (!node.block_confirmed (ancestor))
					{
						return true;
					}
				}
			}
			return false;
		});

		EXPECT_FALSE (error);
		return error || node.ledger.cemented_count () == node.ledger.block_count ();
	}));
	ASSERT_EQ (node.ledger.cemented_count (), node.ledger.block_count ());
	ASSERT_TIMELY (5s, node.active.empty ());
}

// Confirm a complex dependency graph. Uses frontiers confirmation which will fail to
// confirm a frontier optimistically then fallback to pessimistic confirmation.
TEST (node, dependency_graph_frontier)
{
	nano::test::system system;
	nano::node_config config (system.get_available_port ());
	config.frontiers_confirmation = nano::frontiers_confirmation_mode::disabled;
	auto & node1 = *system.add_node (config);
	auto wallet_id1 = node1.wallets.first_wallet_id ();
	config.peering_port = system.get_available_port ();
	config.frontiers_confirmation = nano::frontiers_confirmation_mode::always;
	auto & node2 = *system.add_node (config);
	auto wallet_id2 = node2.wallets.first_wallet_id ();

	nano::state_block_builder builder;
	nano::keypair key1, key2, key3;

	// Send to key1
	auto gen_send1 = builder.make_block ()
					 .account (nano::dev::genesis_key.pub)
					 .previous (nano::dev::genesis->hash ())
					 .representative (nano::dev::genesis_key.pub)
					 .link (key1.pub)
					 .balance (nano::dev::constants.genesis_amount - 1)
					 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
					 .work (*system.work.generate (nano::dev::genesis->hash ()))
					 .build ();
	// Receive from genesis
	auto key1_open = builder.make_block ()
					 .account (key1.pub)
					 .previous (0)
					 .representative (key1.pub)
					 .link (gen_send1->hash ())
					 .balance (1)
					 .sign (key1.prv, key1.pub)
					 .work (*system.work.generate (key1.pub))
					 .build ();
	// Send to genesis
	auto key1_send1 = builder.make_block ()
					  .account (key1.pub)
					  .previous (key1_open->hash ())
					  .representative (key1.pub)
					  .link (nano::dev::genesis_key.pub)
					  .balance (0)
					  .sign (key1.prv, key1.pub)
					  .work (*system.work.generate (key1_open->hash ()))
					  .build ();
	// Receive from key1
	auto gen_receive = builder.make_block ()
					   .from (*gen_send1)
					   .previous (gen_send1->hash ())
					   .link (key1_send1->hash ())
					   .balance (nano::dev::constants.genesis_amount)
					   .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
					   .work (*system.work.generate (gen_send1->hash ()))
					   .build ();
	// Send to key2
	auto gen_send2 = builder.make_block ()
					 .from (*gen_receive)
					 .previous (gen_receive->hash ())
					 .link (key2.pub)
					 .balance (gen_receive->balance_field ().value ().number () - 2)
					 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
					 .work (*system.work.generate (gen_receive->hash ()))
					 .build ();
	// Receive from genesis
	auto key2_open = builder.make_block ()
					 .account (key2.pub)
					 .previous (0)
					 .representative (key2.pub)
					 .link (gen_send2->hash ())
					 .balance (2)
					 .sign (key2.prv, key2.pub)
					 .work (*system.work.generate (key2.pub))
					 .build ();
	// Send to key3
	auto key2_send1 = builder.make_block ()
					  .account (key2.pub)
					  .previous (key2_open->hash ())
					  .representative (key2.pub)
					  .link (key3.pub)
					  .balance (1)
					  .sign (key2.prv, key2.pub)
					  .work (*system.work.generate (key2_open->hash ()))
					  .build ();
	// Receive from key2
	auto key3_open = builder.make_block ()
					 .account (key3.pub)
					 .previous (0)
					 .representative (key3.pub)
					 .link (key2_send1->hash ())
					 .balance (1)
					 .sign (key3.prv, key3.pub)
					 .work (*system.work.generate (key3.pub))
					 .build ();
	// Send to key1
	auto key2_send2 = builder.make_block ()
					  .from (*key2_send1)
					  .previous (key2_send1->hash ())
					  .link (key1.pub)
					  .balance (key2_send1->balance_field ().value ().number () - 1)
					  .sign (key2.prv, key2.pub)
					  .work (*system.work.generate (key2_send1->hash ()))
					  .build ();
	// Receive from key2
	auto key1_receive = builder.make_block ()
						.from (*key1_send1)
						.previous (key1_send1->hash ())
						.link (key2_send2->hash ())
						.balance (key1_send1->balance_field ().value ().number () + 1)
						.sign (key1.prv, key1.pub)
						.work (*system.work.generate (key1_send1->hash ()))
						.build ();
	// Send to key3
	auto key1_send2 = builder.make_block ()
					  .from (*key1_receive)
					  .previous (key1_receive->hash ())
					  .link (key3.pub)
					  .balance (key1_receive->balance_field ().value ().number () - 1)
					  .sign (key1.prv, key1.pub)
					  .work (*system.work.generate (key1_receive->hash ()))
					  .build ();
	// Receive from key1
	auto key3_receive = builder.make_block ()
						.from (*key3_open)
						.previous (key3_open->hash ())
						.link (key1_send2->hash ())
						.balance (key3_open->balance_field ().value ().number () + 1)
						.sign (key3.prv, key3.pub)
						.work (*system.work.generate (key3_open->hash ()))
						.build ();
	// Upgrade key3
	auto key3_epoch = builder.make_block ()
					  .from (*key3_receive)
					  .previous (key3_receive->hash ())
					  .link (node1.ledger.epoch_link (nano::epoch::epoch_1))
					  .balance (key3_receive->balance_field ().value ())
					  .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
					  .work (*system.work.generate (key3_receive->hash ()))
					  .build ();

	for (auto const & node : system.nodes)
	{
		auto transaction (node->store.tx_begin_write ());
		ASSERT_EQ (nano::block_status::progress, node->ledger.process (*transaction, gen_send1));
		ASSERT_EQ (nano::block_status::progress, node->ledger.process (*transaction, key1_open));
		ASSERT_EQ (nano::block_status::progress, node->ledger.process (*transaction, key1_send1));
		ASSERT_EQ (nano::block_status::progress, node->ledger.process (*transaction, gen_receive));
		ASSERT_EQ (nano::block_status::progress, node->ledger.process (*transaction, gen_send2));
		ASSERT_EQ (nano::block_status::progress, node->ledger.process (*transaction, key2_open));
		ASSERT_EQ (nano::block_status::progress, node->ledger.process (*transaction, key2_send1));
		ASSERT_EQ (nano::block_status::progress, node->ledger.process (*transaction, key3_open));
		ASSERT_EQ (nano::block_status::progress, node->ledger.process (*transaction, key2_send2));
		ASSERT_EQ (nano::block_status::progress, node->ledger.process (*transaction, key1_receive));
		ASSERT_EQ (nano::block_status::progress, node->ledger.process (*transaction, key1_send2));
		ASSERT_EQ (nano::block_status::progress, node->ledger.process (*transaction, key3_receive));
		ASSERT_EQ (nano::block_status::progress, node->ledger.process (*transaction, key3_epoch));
	}

	// node1 can vote, but only on the first block
	(void)node1.wallets.insert_adhoc (wallet_id1, nano::dev::genesis_key.prv);

	ASSERT_TIMELY (10s, node2.active.active (gen_send1->qualified_root ()));
	node1.start_election (gen_send1);

	ASSERT_TIMELY_EQ (15s, node1.ledger.cemented_count (), node1.ledger.block_count ());
	ASSERT_TIMELY_EQ (15s, node2.ledger.cemented_count (), node2.ledger.block_count ());
}

namespace nano
{
TEST (node, deferred_dependent_elections)
{
	nano::test::system system;
	nano::node_config node_config_1{ system.get_available_port () };
	node_config_1.frontiers_confirmation = nano::frontiers_confirmation_mode::disabled;
	nano::node_config node_config_2{ system.get_available_port () };
	node_config_2.frontiers_confirmation = nano::frontiers_confirmation_mode::disabled;
	nano::node_flags flags;
	flags.set_disable_request_loop (true);
	auto & node = *system.add_node (node_config_1, flags);
	auto & node2 = *system.add_node (node_config_2, flags); // node2 will be used to ensure all blocks are being propagated

	nano::state_block_builder builder;
	nano::keypair key;
	auto send1 = builder.make_block ()
				 .account (nano::dev::genesis_key.pub)
				 .previous (nano::dev::genesis->hash ())
				 .representative (nano::dev::genesis_key.pub)
				 .link (key.pub)
				 .balance (nano::dev::constants.genesis_amount - 1)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (nano::dev::genesis->hash ()))
				 .build ();
	auto open = builder.make_block ()
				.account (key.pub)
				.previous (0)
				.representative (key.pub)
				.link (send1->hash ())
				.balance (1)
				.sign (key.prv, key.pub)
				.work (*system.work.generate (key.pub))
				.build ();
	auto send2 = builder.make_block ()
				 .from (*send1)
				 .previous (send1->hash ())
				 .balance (send1->balance_field ().value ().number () - 1)
				 .link (key.pub)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (send1->hash ()))
				 .build ();
	auto receive = builder.make_block ()
				   .from (*open)
				   .previous (open->hash ())
				   .link (send2->hash ())
				   .balance (2)
				   .sign (key.prv, key.pub)
				   .work (*system.work.generate (open->hash ()))
				   .build ();
	auto fork = builder.make_block ()
				.from (*receive)
				.representative (nano::dev::genesis_key.pub) // was key.pub
				.sign (key.prv, key.pub)
				.build ();

	nano::test::process (node, { send1 });
	auto election_send1 = nano::test::start_election (system, node, send1->hash ());
	ASSERT_NE (nullptr, election_send1);

	// Should process and republish but not start an election for any dependent blocks
	nano::test::process (node, { open, send2 });
	ASSERT_TIMELY (5s, node.block (open->hash ()));
	ASSERT_TIMELY (5s, node.block (send2->hash ()));
	ASSERT_NEVER (0.5s, node.active.active (open->qualified_root ()) || node.active.active (send2->qualified_root ()));
	ASSERT_TIMELY (5s, node2.block (open->hash ()));
	ASSERT_TIMELY (5s, node2.block (send2->hash ()));

	// Re-processing older blocks with updated work also does not start an election
	node.work_generate_blocking (*open, nano::dev::network_params.work.difficulty (*open) + 1);
	node.process_local (open);
	ASSERT_NEVER (0.5s, node.active.active (open->qualified_root ()));

	// It is however possible to manually start an election from elsewhere
	ASSERT_TRUE (nano::test::start_election (system, node, open->hash ()));
	node.active.erase (*open);
	ASSERT_FALSE (node.active.active (open->qualified_root ()));

	/// The election was dropped but it's still not possible to restart it
	node.work_generate_blocking (*open, nano::dev::network_params.work.difficulty (*open) + 1);
	ASSERT_FALSE (node.active.active (open->qualified_root ()));
	node.process_local (open);
	ASSERT_NEVER (0.5s, node.active.active (open->qualified_root ()));

	// Drop both elections
	node.active.erase (*open);
	ASSERT_FALSE (node.active.active (open->qualified_root ()));
	node.active.erase (*send2);
	ASSERT_FALSE (node.active.active (send2->qualified_root ()));

	// Confirming send1 will automatically start elections for the dependents
	node.active.force_confirm (*election_send1);
	ASSERT_TIMELY (5s, node.block_confirmed (send1->hash ()));
	ASSERT_TIMELY (5s, node.active.active (open->qualified_root ()));
	ASSERT_TIMELY (5s, node.active.active (send2->qualified_root ()));
	auto election_open = node.active.election (open->qualified_root ());
	ASSERT_NE (nullptr, election_open);
	auto election_send2 = node.active.election (send2->qualified_root ());
	ASSERT_NE (nullptr, election_open);

	// Confirm one of the dependents of the receive but not the other, to ensure both have to be confirmed to start an election on processing
	ASSERT_EQ (nano::block_status::progress, node.process (receive));
	ASSERT_FALSE (node.active.active (receive->qualified_root ()));
	node.active.force_confirm (*election_open);
	ASSERT_TIMELY (5s, node.block_confirmed (open->hash ()));
	ASSERT_FALSE (node.ledger.dependents_confirmed (*node.store.tx_begin_read (), *receive));
	ASSERT_NEVER (0.5s, node.active.active (receive->qualified_root ()));
	ASSERT_FALSE (node.ledger.rollback (*node.store.tx_begin_write (), receive->hash ()));
	ASSERT_FALSE (node.block (receive->hash ()));
	node.process_local (receive);
	ASSERT_TIMELY (5s, node.block (receive->hash ()));
	ASSERT_NEVER (0.5s, node.active.active (receive->qualified_root ()));

	// Processing a fork will also not start an election
	ASSERT_EQ (nano::block_status::fork, node.process (fork));
	node.process_local (fork);
	ASSERT_NEVER (0.5s, node.active.active (receive->qualified_root ()));

	// Confirming the other dependency allows starting an election from a fork
	node.active.force_confirm (*election_send2);
	ASSERT_TIMELY (5s, node.block_confirmed (send2->hash ()));
	ASSERT_TIMELY (5s, node.active.active (receive->qualified_root ()));
}
}

// Test that a node configured with `enable_pruning` and `max_pruning_age = 1s` will automatically
// prune old confirmed blocks without explicitly saying `node.ledger_pruning` in the unit test
TEST (node, pruning_automatic)
{
	nano::test::system system{};

	nano::node_config node_config{ system.get_available_port () };
	// TODO: remove after allowing pruned voting
	node_config.enable_voting = false;
	node_config.max_pruning_age = std::chrono::seconds (1);

	nano::node_flags node_flags{};
	node_flags.set_enable_pruning (true);

	auto & node1 = *system.add_node (node_config, node_flags);
	nano::keypair key1{};
	nano::send_block_builder builder{};
	auto latest_hash = nano::dev::genesis->hash ();

	auto send1 = builder.make_block ()
				 .previous (latest_hash)
				 .destination (key1.pub)
				 .balance (nano::dev::constants.genesis_amount - nano::Gxrb_ratio)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (latest_hash))
				 .build ();
	node1.process_active (send1);

	latest_hash = send1->hash ();
	auto send2 = builder.make_block ()
				 .previous (latest_hash)
				 .destination (key1.pub)
				 .balance (0)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (latest_hash))
				 .build ();
	node1.process_active (send2);
	ASSERT_TIMELY (5s, node1.block (send2->hash ()) != nullptr);

	// Force-confirm both blocks
	node1.process_confirmed (nano::election_status{ send1 });
	ASSERT_TIMELY (5s, node1.block_confirmed (send1->hash ()));
	node1.process_confirmed (nano::election_status{ send2 });
	ASSERT_TIMELY (5s, node1.block_confirmed (send2->hash ()));

	// Check pruning result
	ASSERT_EQ (3, node1.ledger.block_count ());
	ASSERT_TIMELY_EQ (5s, node1.ledger.pruned_count (), 1);
	ASSERT_TIMELY_EQ (5s, node1.store.pruned ().count (*node1.store.tx_begin_read ()), 1);
	ASSERT_EQ (1, node1.ledger.pruned_count ());
	ASSERT_EQ (3, node1.ledger.block_count ());

	ASSERT_TRUE (nano::test::block_or_pruned_all_exists (node1, { nano::dev::genesis, send1, send2 }));
}

TEST (node, pruning_age)
{
	nano::test::system system{};

	nano::node_config node_config{ system.get_available_port () };
	// TODO: remove after allowing pruned voting
	node_config.enable_voting = false;
	// Pruning with max age 0
	node_config.max_pruning_age = std::chrono::seconds{ 0 };

	nano::node_flags node_flags{};
	node_flags.set_enable_pruning (true);

	auto & node1 = *system.add_node (node_config, node_flags);
	nano::keypair key1{};
	nano::send_block_builder builder{};
	auto latest_hash = nano::dev::genesis->hash ();

	auto send1 = builder.make_block ()
				 .previous (latest_hash)
				 .destination (key1.pub)
				 .balance (nano::dev::constants.genesis_amount - nano::Gxrb_ratio)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (latest_hash))
				 .build ();
	node1.process_active (send1);

	latest_hash = send1->hash ();
	auto send2 = builder.make_block ()
				 .previous (latest_hash)
				 .destination (key1.pub)
				 .balance (0)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (latest_hash))
				 .build ();
	node1.process_active (send2);

	// Force-confirm both blocks
	node1.process_confirmed (nano::election_status{ send1 });
	ASSERT_TIMELY (5s, node1.block_confirmed (send1->hash ()));
	node1.process_confirmed (nano::election_status{ send2 });
	ASSERT_TIMELY (5s, node1.block_confirmed (send2->hash ()));

	node1.ledger_pruning (1, true);
	ASSERT_EQ (1, node1.ledger.pruned_count ());
	ASSERT_EQ (3, node1.ledger.block_count ());

	ASSERT_TRUE (nano::test::block_or_pruned_all_exists (node1, { nano::dev::genesis, send1, send2 }));
}

// Test that a node configured with `enable_pruning` will
// prune DEEP-enough confirmed blocks by explicitly saying `node.ledger_pruning` in the unit test
TEST (node, pruning_depth)
{
	nano::test::system system{};

	nano::node_config node_config{ system.get_available_port () };
	// TODO: remove after allowing pruned voting
	node_config.enable_voting = false;

	nano::node_flags node_flags{};
	node_flags.set_enable_pruning (true);

	auto & node1 = *system.add_node (node_config, node_flags);
	nano::keypair key1{};
	nano::send_block_builder builder{};
	auto latest_hash = nano::dev::genesis->hash ();

	auto send1 = builder.make_block ()
				 .previous (latest_hash)
				 .destination (key1.pub)
				 .balance (nano::dev::constants.genesis_amount - nano::Gxrb_ratio)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (latest_hash))
				 .build ();
	node1.process_active (send1);

	latest_hash = send1->hash ();
	auto send2 = builder.make_block ()
				 .previous (latest_hash)
				 .destination (key1.pub)
				 .balance (0)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (latest_hash))
				 .build ();
	node1.process_active (send2);

	// Force-confirm both blocks
	node1.process_confirmed (nano::election_status{ send1 });
	ASSERT_TIMELY (5s, node1.block_confirmed (send1->hash ()));
	node1.process_confirmed (nano::election_status{ send2 });
	ASSERT_TIMELY (5s, node1.block_confirmed (send2->hash ()));

	// Three blocks in total, nothing pruned yet
	ASSERT_EQ (0, node1.ledger.pruned_count ());
	ASSERT_EQ (3, node1.ledger.block_count ());

	// Pruning with default depth (unlimited)
	node1.ledger_pruning (1, true);
	ASSERT_EQ (0, node1.ledger.pruned_count ());
	ASSERT_EQ (3, node1.ledger.block_count ());
}

TEST (node, pruning_depth_max_depth)
{
	nano::test::system system{};

	nano::node_config node_config{ system.get_available_port () };
	// TODO: remove after allowing pruned voting
	node_config.enable_voting = false;
	// Pruning with max depth 1
	node_config.max_pruning_depth = 1;

	nano::node_flags node_flags{};
	node_flags.set_enable_pruning (true);

	auto & node1 = *system.add_node (node_config, node_flags);
	nano::keypair key1{};
	nano::send_block_builder builder{};
	auto latest_hash = nano::dev::genesis->hash ();

	auto send1 = builder.make_block ()
				 .previous (latest_hash)
				 .destination (key1.pub)
				 .balance (nano::dev::constants.genesis_amount - nano::Gxrb_ratio)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (latest_hash))
				 .build ();
	node1.process_active (send1);

	latest_hash = send1->hash ();
	auto send2 = builder.make_block ()
				 .previous (latest_hash)
				 .destination (key1.pub)
				 .balance (0)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (latest_hash))
				 .build ();
	node1.process_active (send2);

	// Force-confirm both blocks
	node1.process_confirmed (nano::election_status{ send1 });
	ASSERT_TIMELY (5s, node1.block_confirmed (send1->hash ()));
	node1.process_confirmed (nano::election_status{ send2 });
	ASSERT_TIMELY (5s, node1.block_confirmed (send2->hash ()));

	node1.ledger_pruning (1, true);
	ASSERT_EQ (1, node1.ledger.pruned_count ());
	ASSERT_EQ (3, node1.ledger.block_count ());

	ASSERT_TRUE (nano::test::block_or_pruned_all_exists (node1, { nano::dev::genesis, send1, send2 }));
}
