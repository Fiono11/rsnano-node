#include <nano/lib/blocks.hpp>
#include <nano/lib/config.hpp>
#include <nano/node/network.hpp>
#include <nano/node/nodeconfig.hpp>
#include <nano/node/scheduler/component.hpp>
#include <nano/node/scheduler/priority.hpp>
#include <nano/node/transport/inproc.hpp>
#include <nano/node/transport/socket.hpp>
#include <nano/node/transport/tcp_listener.hpp>
#include <nano/secure/ledger.hpp>
#include <nano/store/component.hpp>
#include <nano/test_common/network.hpp>
#include <nano/test_common/system.hpp>
#include <nano/test_common/testutil.hpp>

#include <gtest/gtest.h>

#include <boost/iostreams/stream_buffer.hpp>
#include <boost/range/join.hpp>
#include <boost/thread.hpp>

using namespace std::chrono_literals;

TEST (network, tcp_connection)
{
	nano::test::system system;
	boost::asio::ip::tcp::acceptor acceptor (system.async_rt.io_ctx);
	auto port = system.get_available_port ();
	boost::asio::ip::tcp::endpoint endpoint (boost::asio::ip::address_v4::any (), port);
	acceptor.open (endpoint.protocol ());
	acceptor.set_option (boost::asio::ip::tcp::acceptor::reuse_address (true));
	acceptor.bind (endpoint);
	acceptor.listen ();
	boost::asio::ip::tcp::socket incoming (system.async_rt.io_ctx);
	std::atomic<bool> done1 (false);
	std::string message1;
	acceptor.async_accept (incoming, [&done1, &message1] (boost::system::error_code const & ec_a) {
		if (ec_a)
		{
			message1 = ec_a.message ();
			std::cerr << message1;
		}
		done1 = true;
	});
	boost::asio::ip::tcp::socket connector (system.async_rt.io_ctx);
	std::atomic<bool> done2 (false);
	std::string message2;
	connector.async_connect (boost::asio::ip::tcp::endpoint (boost::asio::ip::address_v4::loopback (), acceptor.local_endpoint ().port ()),
	[&done2, &message2] (boost::system::error_code const & ec_a) {
		if (ec_a)
		{
			message2 = ec_a.message ();
			std::cerr << message2;
		}
		done2 = true;
	});
	ASSERT_TIMELY (5s, done1 && done2);
	ASSERT_EQ (0, message1.size ());
	ASSERT_EQ (0, message2.size ());
}

TEST (network, construction_with_specified_port)
{
	nano::test::system system{};
	auto const port = nano::test::speculatively_choose_a_free_tcp_bind_port ();
	ASSERT_NE (port, 0);
	auto const node = system.add_node (nano::node_config{ port });
	EXPECT_EQ (port, node->network->port);
	EXPECT_EQ (port, node->network->endpoint ().port ());
	EXPECT_EQ (port, node->tcp_listener->endpoint ().port ());
}

TEST (network, construction_without_specified_port)
{
	nano::test::system system{};
	auto const node = system.add_node ();
	auto const port = node->network->port.load ();
	EXPECT_NE (0, port);
	EXPECT_EQ (port, node->network->endpoint ().port ());
	EXPECT_EQ (port, node->tcp_listener->endpoint ().port ());
}

// Disabled, because it is flakey with Tokio
TEST (DISABLED_network, send_node_id_handshake_tcp)
{
	nano::test::system system (1);
	auto node0 (system.nodes[0]);
	ASSERT_EQ (0, node0->network->size ());
	auto node1 (std::make_shared<nano::node> (system.async_rt, system.get_available_port (), nano::unique_path (), system.work));
	node1->start ();
	system.nodes.push_back (node1);
	auto initial (node0->stats->count (nano::stat::type::message, nano::stat::detail::node_id_handshake, nano::stat::dir::in));
	auto initial_node1 (node1->stats->count (nano::stat::type::message, nano::stat::detail::node_id_handshake, nano::stat::dir::in));
	auto initial_keepalive (node0->stats->count (nano::stat::type::message, nano::stat::detail::keepalive, nano::stat::dir::in));
	std::weak_ptr<nano::node> node_w (node0);
	node0->network->tcp_channels->start_tcp (node1->network->endpoint ());
	ASSERT_EQ (0, node0->network->size ());
	ASSERT_EQ (0, node1->network->size ());
	ASSERT_TIMELY (10s, node0->stats->count (nano::stat::type::message, nano::stat::detail::node_id_handshake, nano::stat::dir::in) >= initial + 2);
	ASSERT_TIMELY (5s, node1->stats->count (nano::stat::type::message, nano::stat::detail::node_id_handshake, nano::stat::dir::in) >= initial_node1 + 2);
	ASSERT_TIMELY (5s, node0->stats->count (nano::stat::type::message, nano::stat::detail::keepalive, nano::stat::dir::in) >= initial_keepalive + 2);
	ASSERT_TIMELY (5s, node1->stats->count (nano::stat::type::message, nano::stat::detail::keepalive, nano::stat::dir::in) >= initial_keepalive + 2);
	ASSERT_EQ (1, node0->network->size ());
	ASSERT_EQ (1, node1->network->size ());
	auto list1 (node0->network->tcp_channels->list (1));
	ASSERT_EQ (nano::transport::transport_type::tcp, list1[0]->get_type ());
	ASSERT_EQ (node1->get_node_id (), list1[0]->get_node_id ());
	auto list2 (node1->network->tcp_channels->list (1));
	ASSERT_EQ (nano::transport::transport_type::tcp, list2[0]->get_type ());
	ASSERT_EQ (node0->get_node_id (), list2[0]->get_node_id ());
}

TEST (network, last_contacted)
{
	nano::test::system system (1);

	auto node0 = system.nodes[0];
	ASSERT_EQ (0, node0->network->size ());

	nano::node_config node1_config = system.default_config ();
	node1_config.tcp_incoming_connections_max = 0; // Prevent ephemeral node1->node0 channel repacement with incoming connection
	auto node1 (std::make_shared<nano::node> (system.async_rt, nano::unique_path (), node1_config, system.work));
	node1->start ();
	system.nodes.push_back (node1);

	auto channel1 = nano::test::establish_tcp (system, *node1, node0->network->endpoint ());
	ASSERT_NE (nullptr, channel1);
	ASSERT_TIMELY_EQ (3s, node0->network->size (), 1);

	// channel0 is the other side of channel1, same connection different endpoint
	auto channel0 = node0->network->tcp_channels->find_node_id (node1->node_id.pub);
	ASSERT_NE (nullptr, channel0);

	{
		// check that the endpoints are part of the same connection
		ASSERT_EQ (channel0->get_local_endpoint (), channel1->get_tcp_remote_endpoint ());
		ASSERT_EQ (channel1->get_local_endpoint (), channel0->get_tcp_remote_endpoint ());
	}

	// capture the state before and ensure the clock ticks at least once
	auto timestamp_before_keepalive = channel0->get_last_packet_received ();
	auto keepalive_count = node0->stats->count (nano::stat::type::message, nano::stat::detail::keepalive, nano::stat::dir::in);
	ASSERT_TIMELY (3s, std::chrono::system_clock::now () > timestamp_before_keepalive);

	// send 3 keepalives
	// we need an extra keepalive to handle the race condition between the timestamp set and the counter increment
	// and we need one more keepalive to handle the possibility that there is a keepalive already in flight when we start the crucial part of the test
	// it is possible that there could be multiple keepalives in flight but we assume here that there will be no more than one in flight for the purposes of this test
	node1->network->send_keepalive (channel1);
	node1->network->send_keepalive (channel1);
	node1->network->send_keepalive (channel1);

	ASSERT_TIMELY (3s, node0->stats->count (nano::stat::type::message, nano::stat::detail::keepalive, nano::stat::dir::in) >= keepalive_count + 3);
	ASSERT_EQ (node0->network->size (), 1);
	auto timestamp_after_keepalive = channel0->get_last_packet_received ();
	ASSERT_GT (timestamp_after_keepalive, timestamp_before_keepalive);
}

TEST (network, multi_keepalive)
{
	nano::test::system system (1);
	auto node0 = system.nodes[0];
	ASSERT_EQ (0, node0->network->size ());
	auto node1 (std::make_shared<nano::node> (system.async_rt, system.get_available_port (), nano::unique_path (), system.work));
	ASSERT_FALSE (node1->init_error ());
	node1->start ();
	system.nodes.push_back (node1);
	ASSERT_EQ (0, node1->network->size ());
	ASSERT_EQ (0, node0->network->size ());
	node1->network->tcp_channels->start_tcp (node0->network->endpoint ());
	ASSERT_TIMELY (10s, node0->network->size () == 1 && node0->stats->count (nano::stat::type::message, nano::stat::detail::keepalive) >= 1);
	auto node2 (std::make_shared<nano::node> (system.async_rt, system.get_available_port (), nano::unique_path (), system.work));
	ASSERT_FALSE (node2->init_error ());
	node2->start ();
	system.nodes.push_back (node2);
	node2->network->tcp_channels->start_tcp (node0->network->endpoint ());
	ASSERT_TIMELY (10s, node1->network->size () == 2 && node0->network->size () == 2 && node2->network->size () == 2 && node0->stats->count (nano::stat::type::message, nano::stat::detail::keepalive) >= 2);
}

TEST (network, send_discarded_publish)
{
	nano::test::system system (2);
	auto & node1 (*system.nodes[0]);
	auto & node2 (*system.nodes[1]);
	nano::keypair key1;
	nano::block_builder builder;
	auto block = builder
				 .send ()
				 .previous (1)
				 .destination (1)
				 .balance (2)
				 .sign (key1.prv, key1.pub)
				 .work (*system.work.generate (nano::root (1)))
				 .build ();
	{
		auto transaction (node1.store.tx_begin_read ());
		node1.network->flood_block (block);
		ASSERT_EQ (nano::dev::genesis->hash (), node1.ledger.latest (*transaction, nano::dev::genesis_key.pub));
		ASSERT_EQ (nano::dev::genesis->hash (), node2.latest (nano::dev::genesis_key.pub));
	}
	ASSERT_TIMELY (10s, node2.stats->count (nano::stat::type::message, nano::stat::detail::publish, nano::stat::dir::in) != 0);
	auto transaction (node1.store.tx_begin_read ());
	ASSERT_EQ (nano::dev::genesis->hash (), node1.ledger.latest (*transaction, nano::dev::genesis_key.pub));
	ASSERT_EQ (nano::dev::genesis->hash (), node2.latest (nano::dev::genesis_key.pub));
}

TEST (network, send_invalid_publish)
{
	nano::test::system system (2);
	auto & node1 (*system.nodes[0]);
	auto & node2 (*system.nodes[1]);
	nano::block_builder builder;
	auto block = builder
				 .send ()
				 .previous (1)
				 .destination (1)
				 .balance (20)
				 .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				 .work (*system.work.generate (nano::root (1)))
				 .build ();
	{
		auto transaction (node1.store.tx_begin_read ());
		node1.network->flood_block (block);
		ASSERT_EQ (nano::dev::genesis->hash (), node1.ledger.latest (*transaction, nano::dev::genesis_key.pub));
		ASSERT_EQ (nano::dev::genesis->hash (), node2.latest (nano::dev::genesis_key.pub));
	}
	ASSERT_TIMELY (10s, node2.stats->count (nano::stat::type::message, nano::stat::detail::publish, nano::stat::dir::in) != 0);
	auto transaction (node1.store.tx_begin_read ());
	ASSERT_EQ (nano::dev::genesis->hash (), node1.ledger.latest (*transaction, nano::dev::genesis_key.pub));
	ASSERT_EQ (nano::dev::genesis->hash (), node2.latest (nano::dev::genesis_key.pub));
}

TEST (network, send_valid_confirm_ack)
{
	auto type = nano::transport::transport_type::tcp;
	nano::node_flags node_flags;
	nano::test::system system (2, type, node_flags);
	auto & node1 (*system.nodes[0]);
	auto & node2 (*system.nodes[1]);
	auto wallet_id1 = node1.wallets.first_wallet_id ();
	auto wallet_id2 = node2.wallets.first_wallet_id ();
	nano::keypair key2;
	(void)node1.wallets.insert_adhoc (wallet_id1, nano::dev::genesis_key.prv);
	(void)node2.wallets.insert_adhoc (wallet_id2, key2.prv);
	nano::block_hash latest1 (node1.latest (nano::dev::genesis_key.pub));
	nano::block_builder builder;
	auto block2 = builder
				  .send ()
				  .previous (latest1)
				  .destination (key2.pub)
				  .balance (50)
				  .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				  .work (*system.work.generate (latest1))
				  .build ();
	nano::block_hash latest2 (node2.latest (nano::dev::genesis_key.pub));
	node1.process_active (std::make_shared<nano::send_block> (*block2));
	// Keep polling until latest block changes
	ASSERT_TIMELY (10s, node2.latest (nano::dev::genesis_key.pub) != latest2);
	// Make sure the balance has decreased after processing the block.
	ASSERT_EQ (50, node2.balance (nano::dev::genesis_key.pub));
}

TEST (network, send_valid_publish)
{
	auto type = nano::transport::transport_type::tcp;
	nano::node_flags node_flags;
	nano::test::system system (2, type, node_flags);
	auto & node1 (*system.nodes[0]);
	auto & node2 (*system.nodes[1]);
	auto wallet_id1 = node1.wallets.first_wallet_id ();
	auto wallet_id2 = node2.wallets.first_wallet_id ();
	node1.bootstrap_initiator.stop ();
	node2.bootstrap_initiator.stop ();
	(void)node1.wallets.insert_adhoc (wallet_id1, nano::dev::genesis_key.prv);
	nano::keypair key2;
	(void)node2.wallets.insert_adhoc (wallet_id2, key2.prv);
	nano::block_hash latest1 (node1.latest (nano::dev::genesis_key.pub));
	nano::block_builder builder;
	auto block2 = builder
				  .send ()
				  .previous (latest1)
				  .destination (key2.pub)
				  .balance (50)
				  .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				  .work (*system.work.generate (latest1))
				  .build ();
	auto hash2 (block2->hash ());
	nano::block_hash latest2 (node2.latest (nano::dev::genesis_key.pub));
	node2.process_active (std::make_shared<nano::send_block> (*block2));
	ASSERT_TIMELY (10s, node1.stats->count (nano::stat::type::message, nano::stat::detail::publish, nano::stat::dir::in) != 0);
	ASSERT_NE (hash2, latest2);
	ASSERT_TIMELY (10s, node2.latest (nano::dev::genesis_key.pub) != latest2);
	ASSERT_EQ (50, node2.balance (nano::dev::genesis_key.pub));
}

TEST (network, send_insufficient_work)
{
	nano::test::system system (2);
	auto & node1 = *system.nodes[0];
	auto & node2 = *system.nodes[1];
	// Block zero work
	nano::block_builder builder;
	auto block1 = builder
				  .send ()
				  .previous (0)
				  .destination (1)
				  .balance (20)
				  .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				  .work (0)
				  .build ();
	nano::publish publish1{ nano::dev::network_params.network, block1 };
	auto tcp_channel (node1.network->tcp_channels->find_node_id (node2.get_node_id ()));
	ASSERT_NE (nullptr, tcp_channel);
	tcp_channel->send (publish1, [] (boost::system::error_code const & ec, size_t size) {});
	ASSERT_EQ (0, node1.stats->count (nano::stat::type::error, nano::stat::detail::insufficient_work));
	ASSERT_TIMELY (10s, node2.stats->count (nano::stat::type::error, nano::stat::detail::insufficient_work) != 0);
	ASSERT_EQ (1, node2.stats->count (nano::stat::type::error, nano::stat::detail::insufficient_work));
	// Legacy block work between epoch_2_recieve & epoch_1
	auto block2 = builder
				  .send ()
				  .previous (block1->hash ())
				  .destination (1)
				  .balance (20)
				  .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				  .work (system.work_generate_limited (block1->hash (), node1.network_params.work.get_epoch_2_receive (), node1.network_params.work.get_epoch_1 () - 1))
				  .build ();
	nano::publish publish2{ nano::dev::network_params.network, block2 };
	tcp_channel->send (publish2, [] (boost::system::error_code const & ec, size_t size) {});
	ASSERT_TIMELY (10s, node2.stats->count (nano::stat::type::error, nano::stat::detail::insufficient_work) != 1);
	ASSERT_EQ (2, node2.stats->count (nano::stat::type::error, nano::stat::detail::insufficient_work));
	// Legacy block work epoch_1
	auto block3 = builder
				  .send ()
				  .previous (block2->hash ())
				  .destination (1)
				  .balance (20)
				  .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				  .work (*system.work.generate (block2->hash (), node1.network_params.work.get_epoch_2 ()))
				  .build ();
	nano::publish publish3{ nano::dev::network_params.network, block3 };
	tcp_channel->send (publish3, [] (boost::system::error_code const & ec, size_t size) {});
	ASSERT_EQ (0, node2.stats->count (nano::stat::type::message, nano::stat::detail::publish, nano::stat::dir::in));
	ASSERT_TIMELY (10s, node2.stats->count (nano::stat::type::message, nano::stat::detail::publish, nano::stat::dir::in) != 0);
	ASSERT_EQ (1, node2.stats->count (nano::stat::type::message, nano::stat::detail::publish, nano::stat::dir::in));
	// State block work epoch_2_recieve
	auto block4 = builder
				  .state ()
				  .account (nano::dev::genesis_key.pub)
				  .previous (block1->hash ())
				  .representative (nano::dev::genesis_key.pub)
				  .balance (20)
				  .link (1)
				  .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				  .work (system.work_generate_limited (block1->hash (), node1.network_params.work.get_epoch_2_receive (), node1.network_params.work.get_epoch_1 () - 1))
				  .build ();
	nano::publish publish4{ nano::dev::network_params.network, block4 };
	tcp_channel->send (publish4, [] (boost::system::error_code const & ec, size_t size) {});
	ASSERT_TIMELY (10s, node2.stats->count (nano::stat::type::message, nano::stat::detail::publish, nano::stat::dir::in) != 0);
	ASSERT_EQ (1, node2.stats->count (nano::stat::type::message, nano::stat::detail::publish, nano::stat::dir::in));
	ASSERT_EQ (2, node2.stats->count (nano::stat::type::error, nano::stat::detail::insufficient_work));
}

TEST (receivable_processor, confirm_insufficient_pos)
{
	nano::test::system system (1);
	auto & node1 (*system.nodes[0]);
	nano::block_builder builder;
	auto block1 = builder
				  .send ()
				  .previous (nano::dev::genesis->hash ())
				  .destination (0)
				  .balance (0)
				  .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				  .work (0)
				  .build ();
	node1.work_generate_blocking (*block1);
	ASSERT_EQ (nano::block_status::progress, node1.process (block1));
	{
		auto tx{ node1.store.tx_begin_read () };
		node1.scheduler.priority.activate (nano::dev::genesis_key.pub, *tx);
	}
	nano::keypair key1;
	auto vote = nano::test::make_vote (key1, { block1 }, 0, 0);
	nano::confirm_ack con1{ nano::dev::network_params.network, vote };
	auto channel1 = std::make_shared<nano::transport::inproc::channel> (node1, node1);
	node1.network->inbound (con1, channel1);
}

TEST (receivable_processor, confirm_sufficient_pos)
{
	nano::test::system system (1);
	auto & node1 (*system.nodes[0]);
	nano::block_builder builder;
	auto block1 = builder
				  .send ()
				  .previous (nano::dev::genesis->hash ())
				  .destination (0)
				  .balance (0)
				  .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				  .work (0)
				  .build ();
	node1.work_generate_blocking (*block1);
	ASSERT_EQ (nano::block_status::progress, node1.process (block1));
	{
		auto tx{ node1.store.tx_begin_read () };
		node1.scheduler.priority.activate (nano::dev::genesis_key.pub, *tx);
	}
	auto vote = nano::test::make_vote (nano::dev::genesis_key, { block1 }, 0, 0);
	nano::confirm_ack con1{ nano::dev::network_params.network, vote };
	auto channel1 = std::make_shared<nano::transport::inproc::channel> (node1, node1);
	node1.network->inbound (con1, channel1);
}

TEST (receivable_processor, send_with_receive)
{
	auto type = nano::transport::transport_type::tcp;
	nano::node_flags node_flags;
	nano::test::system system (2, type, node_flags);
	auto & node1 (*system.nodes[0]);
	auto & node2 (*system.nodes[1]);
	auto wallet_id1 = node1.wallets.first_wallet_id ();
	auto wallet_id2 = node2.wallets.first_wallet_id ();
	auto amount (std::numeric_limits<nano::uint128_t>::max ());
	nano::keypair key2;
	(void)node1.wallets.insert_adhoc (wallet_id1, nano::dev::genesis_key.prv);
	nano::block_hash latest1 (node1.latest (nano::dev::genesis_key.pub));
	(void)node2.wallets.insert_adhoc (wallet_id2, key2.prv);
	nano::block_builder builder;
	auto block1 = builder
				  .send ()
				  .previous (latest1)
				  .destination (key2.pub)
				  .balance (amount - node1.config->receive_minimum.number ())
				  .sign (nano::dev::genesis_key.prv, nano::dev::genesis_key.pub)
				  .work (*system.work.generate (latest1))
				  .build ();
	ASSERT_EQ (amount, node1.balance (nano::dev::genesis_key.pub));
	ASSERT_EQ (0, node1.balance (key2.pub));
	ASSERT_EQ (amount, node2.balance (nano::dev::genesis_key.pub));
	ASSERT_EQ (0, node2.balance (key2.pub));
	node1.process_active (block1);
	ASSERT_TIMELY (5s, nano::test::exists (node1, { block1 }));
	node2.process_active (block1);
	ASSERT_TIMELY (5s, nano::test::exists (node2, { block1 }));
	ASSERT_EQ (amount - node1.config->receive_minimum.number (), node1.balance (nano::dev::genesis_key.pub));
	ASSERT_EQ (0, node1.balance (key2.pub));
	ASSERT_EQ (amount - node1.config->receive_minimum.number (), node2.balance (nano::dev::genesis_key.pub));
	ASSERT_EQ (0, node2.balance (key2.pub));
	ASSERT_TIMELY (10s, node1.balance (key2.pub) == node1.config->receive_minimum.number () && node2.balance (key2.pub) == node1.config->receive_minimum.number ());
	ASSERT_EQ (amount - node1.config->receive_minimum.number (), node1.balance (nano::dev::genesis_key.pub));
	ASSERT_EQ (node1.config->receive_minimum.number (), node1.balance (key2.pub));
	ASSERT_EQ (amount - node1.config->receive_minimum.number (), node2.balance (nano::dev::genesis_key.pub));
	ASSERT_EQ (node1.config->receive_minimum.number (), node2.balance (key2.pub));
}

TEST (network, receive_weight_change)
{
	nano::test::system system (2);
	auto node1 = system.nodes[0];
	auto node2 = system.nodes[1];
	auto wallet_id1 = node1->wallets.first_wallet_id ();
	auto wallet_id2 = node2->wallets.first_wallet_id ();
	(void)node1->wallets.insert_adhoc (wallet_id1, nano::dev::genesis_key.prv);
	nano::keypair key2;
	(void)node2->wallets.insert_adhoc (wallet_id2, key2.prv);
	(void)node2->wallets.set_representative (wallet_id2, key2.pub);
	ASSERT_NE (nullptr, node1->wallets.send_action (wallet_id1, nano::dev::genesis_key.pub, key2.pub, system.nodes[0]->config->receive_minimum.number ()));
	ASSERT_TIMELY (10s, std::all_of (system.nodes.begin (), system.nodes.end (), [&] (std::shared_ptr<nano::node> const & node_a) { return node_a->weight (key2.pub) == system.nodes[0]->config->receive_minimum.number (); }));
}

TEST (parse_endpoint, valid)
{
	std::string string ("::1:24000");
	nano::endpoint endpoint;
	ASSERT_FALSE (nano::parse_endpoint (string, endpoint));
	ASSERT_EQ (boost::asio::ip::address_v6::loopback (), endpoint.address ());
	ASSERT_EQ (24000, endpoint.port ());
}

TEST (parse_endpoint, invalid_port)
{
	std::string string ("::1:24a00");
	nano::endpoint endpoint;
	ASSERT_TRUE (nano::parse_endpoint (string, endpoint));
}

TEST (parse_endpoint, invalid_address)
{
	std::string string ("::q:24000");
	nano::endpoint endpoint;
	ASSERT_TRUE (nano::parse_endpoint (string, endpoint));
}

TEST (parse_endpoint, no_address)
{
	std::string string (":24000");
	nano::endpoint endpoint;
	ASSERT_TRUE (nano::parse_endpoint (string, endpoint));
}

TEST (parse_endpoint, no_port)
{
	std::string string ("::1:");
	nano::endpoint endpoint;
	ASSERT_TRUE (nano::parse_endpoint (string, endpoint));
}

TEST (parse_endpoint, no_colon)
{
	std::string string ("::1");
	nano::endpoint endpoint;
	ASSERT_TRUE (nano::parse_endpoint (string, endpoint));
}

TEST (network, ipv6)
{
	boost::asio::ip::address_v6 address (boost::asio::ip::make_address_v6 ("::ffff:127.0.0.1"));
	ASSERT_TRUE (address.is_v4_mapped ());
	nano::endpoint endpoint1 (address, 16384);
	std::vector<uint8_t> bytes1;
	{
		nano::vectorstream stream (bytes1);
		nano::write (stream, address.to_bytes ());
	}
	ASSERT_EQ (16, bytes1.size ());
	for (auto i (bytes1.begin ()), n (bytes1.begin () + 10); i != n; ++i)
	{
		ASSERT_EQ (0, *i);
	}
	ASSERT_EQ (0xff, bytes1[10]);
	ASSERT_EQ (0xff, bytes1[11]);
	std::array<uint8_t, 16> bytes2;
	nano::bufferstream stream (bytes1.data (), bytes1.size ());
	auto error (nano::try_read (stream, bytes2));
	ASSERT_FALSE (error);
	nano::endpoint endpoint2 (boost::asio::ip::address_v6 (bytes2), 16384);
	ASSERT_EQ (endpoint1, endpoint2);
}

TEST (network, ipv6_from_ipv4)
{
	nano::endpoint endpoint1 (boost::asio::ip::address_v4::loopback (), 16000);
	ASSERT_TRUE (endpoint1.address ().is_v4 ());
	nano::endpoint endpoint2 (boost::asio::ip::address_v6::v4_mapped (endpoint1.address ().to_v4 ()), 16000);
	ASSERT_TRUE (endpoint2.address ().is_v6 ());
}

TEST (network, reserved_address)
{
	nano::test::system system (1);
	// 0 port test
	ASSERT_TRUE (nano::transport::reserved_address (nano::endpoint (boost::asio::ip::make_address_v6 ("2001::"), 0)));
	// Valid address test
	ASSERT_FALSE (nano::transport::reserved_address (nano::endpoint (boost::asio::ip::make_address_v6 ("2001::"), 1)));
	nano::endpoint loopback (boost::asio::ip::make_address_v6 ("::1"), 1);
	ASSERT_FALSE (nano::transport::reserved_address (loopback));
	nano::endpoint private_network_peer (boost::asio::ip::make_address_v6 ("::ffff:10.0.0.0"), 1);
	ASSERT_TRUE (nano::transport::reserved_address (private_network_peer, false));
	ASSERT_FALSE (nano::transport::reserved_address (private_network_peer, true));
}

TEST (network, ipv6_bind_subnetwork)
{
	auto address1 (boost::asio::ip::make_address_v6 ("a41d:b7b2:8298:cf45:672e:bd1a:e7fb:f713"));
	auto subnet1 (boost::asio::ip::make_network_v6 (address1, 48));
	ASSERT_EQ (boost::asio::ip::make_address_v6 ("a41d:b7b2:8298::"), subnet1.network ());
	auto address1_subnet (nano::transport::ipv4_address_or_ipv6_subnet (address1));
	ASSERT_EQ (subnet1.network (), address1_subnet);
	// Ipv4 should return initial address
	auto address2 (boost::asio::ip::make_address_v6 ("::ffff:192.168.1.1"));
	auto address2_subnet (nano::transport::ipv4_address_or_ipv6_subnet (address2));
	ASSERT_EQ (address2, address2_subnet);
}

TEST (network, network_range_ipv6)
{
	auto address1 (boost::asio::ip::make_address_v6 ("a41d:b7b2:8298:cf45:672e:bd1a:e7fb:f713"));
	auto subnet1 (boost::asio::ip::make_network_v6 (address1, 58));
	ASSERT_EQ (boost::asio::ip::make_address_v6 ("a41d:b7b2:8298:cf40::"), subnet1.network ());
	auto address2 (boost::asio::ip::make_address_v6 ("520d:2402:3d:5e65:11:f8:7c54:3f"));
	auto subnet2 (boost::asio::ip::make_network_v6 (address2, 33));
	ASSERT_EQ (boost::asio::ip::make_address_v6 ("520d:2402:0::"), subnet2.network ());
	// Default settings test
	auto address3 (boost::asio::ip::make_address_v6 ("a719:0f12:536e:d88a:1331:ba53:4598:04e5"));
	auto subnet3 (boost::asio::ip::make_network_v6 (address3, 32));
	ASSERT_EQ (boost::asio::ip::make_address_v6 ("a719:0f12::"), subnet3.network ());
	auto address3_subnet (nano::transport::map_address_to_subnetwork (address3));
	ASSERT_EQ (subnet3.network (), address3_subnet);
}

TEST (network, network_range_ipv4)
{
	auto address1 (boost::asio::ip::make_address_v6 ("::ffff:192.168.1.1"));
	auto subnet1 (boost::asio::ip::make_network_v6 (address1, 96 + 16));
	ASSERT_EQ (boost::asio::ip::make_address_v6 ("::ffff:192.168.0.0"), subnet1.network ());
	// Default settings test
	auto address2 (boost::asio::ip::make_address_v6 ("::ffff:80.67.148.225"));
	auto subnet2 (boost::asio::ip::make_network_v6 (address2, 96 + 24));
	ASSERT_EQ (boost::asio::ip::make_address_v6 ("::ffff:80.67.148.0"), subnet2.network ());
	auto address2_subnet (nano::transport::map_address_to_subnetwork (address2));
	ASSERT_EQ (subnet2.network (), address2_subnet);
}

// Test disabled because it's failing intermittently.
// PR in which it got disabled: https://github.com/nanocurrency/nano-node/pull/3611
// Issue for investigating it: https://github.com/nanocurrency/nano-node/issues/3615
TEST (tcp_listener, DISABLED_tcp_listener_timeout_empty)
{
	nano::test::system system (1);
	auto node0 (system.nodes[0]);
	auto socket (nano::transport::create_client_socket (*node0));
	std::atomic<bool> connected (false);
	socket->async_connect (node0->tcp_listener->endpoint (), [&connected] (boost::system::error_code const & ec) {
		ASSERT_FALSE (ec);
		connected = true;
	});
	ASSERT_TIMELY (5s, connected);
	bool disconnected (false);
	system.deadline_set (std::chrono::seconds (6));
	while (!disconnected)
	{
		disconnected = node0->tcp_listener->connections_count () == 0;
		ASSERT_NO_ERROR (system.poll ());
	}
}

TEST (tcp_listener, tcp_listener_timeout_node_id_handshake)
{
	nano::test::system system (1);
	auto node0 (system.nodes[0]);
	auto socket (nano::transport::create_client_socket (*node0));
	auto cookie (node0->network->syn_cookies->assign (nano::transport::map_tcp_to_endpoint (node0->tcp_listener->endpoint ())));
	ASSERT_TRUE (cookie);
	nano::node_id_handshake::query_payload query{ *cookie };
	nano::node_id_handshake node_id_handshake{ nano::dev::network_params.network, query };
	auto channel = std::make_shared<nano::transport::channel_tcp> (
	node0->async_rt,
	node0->outbound_limiter,
	node0->config->network_params.network,
	socket,
	*node0->stats,
	*node0->network->tcp_channels,
	1);
	socket->async_connect (node0->tcp_listener->endpoint (), [&node_id_handshake, channel] (boost::system::error_code const & ec) {
		ASSERT_FALSE (ec);
		channel->send (node_id_handshake, [] (boost::system::error_code const & ec, size_t size_a) {
			ASSERT_FALSE (ec);
		});
	});
	ASSERT_TIMELY (5s, node0->stats->count (nano::stat::type::tcp_server, nano::stat::detail::node_id_handshake) != 0);
	ASSERT_EQ (node0->tcp_listener->connections_count (), 1);
	bool disconnected (false);
	system.deadline_set (std::chrono::seconds (20));
	while (!disconnected)
	{
		disconnected = node0->tcp_listener->connections_count () == 0;
		ASSERT_NO_ERROR (system.poll ());
	}
}

// Test disabled because it's failing repeatedly for Windows + LMDB.
// PR in which it got disabled: https://github.com/nanocurrency/nano-node/pull/3622
// Issue for investigating it: https://github.com/nanocurrency/nano-node/issues/3621
#ifndef _WIN32
// Disabled, because it does not work with Tokio, because Tokio executes the async requests
// and this test assumes that the async runtime doesn't poll. Test must be rewritten!
TEST (DISABLED_network, peer_max_tcp_attempts)
{
	// Add nodes that can accept TCP connection, but not node ID handshake
	nano::node_flags node_flags;
	node_flags.set_disable_connection_cleanup (true);
	nano::test::system system;
	auto node = system.add_node (node_flags);
	for (auto i (0); i < node->network_params.network.max_peers_per_ip; ++i)
	{
		auto node2 (std::make_shared<nano::node> (system.async_rt, system.get_available_port (), nano::unique_path (), system.work, node_flags));
		node2->start ();
		system.nodes.push_back (node2);
		// Start TCP attempt
		node->network->merge_peer (node2->network->endpoint ());
	}
	ASSERT_EQ (0, node->network->size ());
	ASSERT_FALSE (node->network->tcp_channels->track_reachout (nano::endpoint (node->network->endpoint ().address (), system.get_available_port ())));
	ASSERT_EQ (1, node->stats->count (nano::stat::type::tcp, nano::stat::detail::tcp_max_per_ip, nano::stat::dir::out));
}
#endif

namespace nano
{
namespace transport
{
	TEST (network, peer_max_tcp_attempts_subnetwork)
	{
		nano::node_flags node_flags;
		node_flags.set_disable_max_peers_per_ip (true);
		nano::test::system system;
		system.add_node (node_flags);
		auto node (system.nodes[0]);
		for (auto i (0); i < node->network_params.network.max_peers_per_subnetwork; ++i)
		{
			auto address (boost::asio::ip::address_v6::v4_mapped (boost::asio::ip::address_v4 (0x7f000001 + i))); // 127.0.0.1 hex
			nano::endpoint endpoint (address, system.get_available_port ());
			ASSERT_TRUE (node->network->tcp_channels->track_reachout (endpoint));
		}
		ASSERT_EQ (0, node->network->size ());
		ASSERT_EQ (0, node->stats->count (nano::stat::type::tcp, nano::stat::detail::tcp_max_per_subnetwork, nano::stat::dir::out));
		ASSERT_FALSE (node->network->tcp_channels->track_reachout (nano::endpoint (boost::asio::ip::make_address_v6 ("::ffff:127.0.0.1"), system.get_available_port ())));
		ASSERT_EQ (1, node->stats->count (nano::stat::type::tcp, nano::stat::detail::tcp_max_per_subnetwork, nano::stat::dir::out));
	}
}
}

// Send two publish messages and asserts that the duplication is detected.
TEST (network, duplicate_detection)
{
	nano::test::system system;
	nano::node_flags node_flags;
	auto & node0 = *system.add_node (node_flags);
	auto & node1 = *system.add_node (node_flags);
	nano::publish publish{ nano::dev::network_params.network, nano::dev::genesis };

	ASSERT_EQ (0, node1.stats->count (nano::stat::type::filter, nano::stat::detail::duplicate_publish_message));

	// Publish duplicate detection through TCP
	auto tcp_channel = node0.network->tcp_channels->find_node_id (node1.get_node_id ());
	ASSERT_NE (nullptr, tcp_channel);
	ASSERT_EQ (0, node1.stats->count (nano::stat::type::filter, nano::stat::detail::duplicate_publish_message));
	tcp_channel->send (publish);
	ASSERT_TIMELY_EQ (2s, node1.stats->count (nano::stat::type::filter, nano::stat::detail::duplicate_publish_message), 0);
	tcp_channel->send (publish);
	ASSERT_TIMELY_EQ (2s, node1.stats->count (nano::stat::type::filter, nano::stat::detail::duplicate_publish_message), 1);
}

TEST (network, duplicate_revert_publish)
{
	nano::test::system system;
	nano::node_config node_config = system.default_config ();
	node_config.block_processor.max_peer_queue = 0;
	auto & node (*system.add_node (node_config));

	nano::publish publish{ nano::dev::network_params.network, nano::dev::genesis };
	std::vector<uint8_t> bytes;
	{
		nano::vectorstream stream (bytes);
		publish.get_block ()->serialize (stream);
	}
	// Add to the blocks filter
	// Should be cleared when dropping due to a full block processor, as long as the message has the optional digest attached
	// Test network.duplicate_detection ensures that the digest is attached when deserializing messages
	nano::uint128_t digest;
	ASSERT_FALSE (node.network->tcp_channels->publish_filter->apply (bytes.data (), bytes.size (), &digest));
	ASSERT_TRUE (node.network->tcp_channels->publish_filter->apply (bytes.data (), bytes.size ()));
	auto other_node (std::make_shared<nano::node> (system.async_rt, system.get_available_port (), nano::unique_path (), system.work));
	other_node->start ();
	system.nodes.push_back (other_node);
	auto channel = nano::test::establish_tcp (system, *other_node, node.network->endpoint ());
	ASSERT_NE (nullptr, channel);
	ASSERT_EQ (0, publish.get_digest ());
	node.network->inbound (publish, channel);
	ASSERT_TRUE (node.network->tcp_channels->publish_filter->apply (bytes.data (), bytes.size ()));
	publish.set_digest (digest);
	node.network->inbound (publish, channel);
	ASSERT_FALSE (node.network->tcp_channels->publish_filter->apply (bytes.data (), bytes.size ()));
}

namespace nano
{
TEST (peer_exclusion, container_info)
{
	nano::peer_exclusion excluded_peers;
	nano::tcp_endpoint endpoint (boost::asio::ip::address_v6::v4_mapped (boost::asio::ip::address_v4 (0x1)), 0);
	ASSERT_EQ (1, excluded_peers.add (endpoint));
	auto component{ excluded_peers.collect_container_info ("") };
	auto composite (dynamic_cast<nano::container_info_composite *> (component.get ()));
	ASSERT_NE (nullptr, component);
	auto children (composite->get_children ());
	ASSERT_EQ (1, children.size ());
	auto child_leaf (dynamic_cast<nano::container_info_leaf *> (children.front ().get ()));
	ASSERT_NE (nullptr, child_leaf);
	auto child_info (child_leaf->get_info ());
	ASSERT_EQ ("peers", child_info.name);
	ASSERT_EQ (1, child_info.count);
	ASSERT_EQ (rsnano::rsn_peer_exclusion_element_size (), child_info.sizeof_element);
}
}

TEST (network, tcp_no_connect_excluded_peers)
{
	nano::test::system system (1);
	auto node0 (system.nodes[0]);
	ASSERT_EQ (0, node0->network->size ());
	auto node1 (std::make_shared<nano::node> (system.async_rt, system.get_available_port (), nano::unique_path (), system.work));
	node1->start ();
	system.nodes.push_back (node1);
	auto endpoint1_tcp (nano::transport::map_endpoint_to_tcp (node1->network->endpoint ()));
	while (!node0->network->tcp_channels->excluded_peers ().check (endpoint1_tcp))
	{
		node0->network->tcp_channels->excluded_peers ().add (endpoint1_tcp);
	}
	ASSERT_EQ (0, node0->stats->count (nano::stat::type::tcp, nano::stat::detail::tcp_excluded));
	node1->network->merge_peer (node0->network->endpoint ());
	ASSERT_TIMELY (5s, node0->stats->count (nano::stat::type::tcp, nano::stat::detail::tcp_excluded) >= 1);
	ASSERT_EQ (nullptr, node0->network->find_node_id (node1->get_node_id ()));

	// Should not actively reachout to excluded peers
	ASSERT_FALSE (node0->network->track_reachout (node1->network->endpoint ()));

	// Erasing from excluded peers should allow a connection
	node0->network->tcp_channels->excluded_peers ().remove (endpoint1_tcp);
	ASSERT_FALSE (node0->network->tcp_channels->excluded_peers ().check (endpoint1_tcp));

	// Wait until there is a syn_cookie
	ASSERT_TIMELY (5s, node1->network->syn_cookies->cookies_size () != 0);

	// Manually cleanup previous attempt
	node1->network->cleanup (std::chrono::system_clock::now ());
	node1->network->syn_cookies->purge (std::chrono::seconds{ 0 });

	// Ensure a successful connection
	ASSERT_EQ (0, node0->network->size ());
	node1->network->merge_peer (node0->network->endpoint ());
	ASSERT_TIMELY_EQ (5s, node0->network->size (), 1);
}

TEST (network, cleanup_purge)
{
	auto test_start = std::chrono::system_clock::now ();

	nano::test::system system (1);
	auto & node1 (*system.nodes[0]);

	auto node2 (std::make_shared<nano::node> (system.async_rt, system.get_available_port (), nano::unique_path (), system.work));
	node2->start ();
	system.nodes.push_back (node2);

	ASSERT_EQ (0, node1.network->size ());
	node1.network->cleanup (test_start);
	ASSERT_EQ (0, node1.network->size ());

	node1.network->cleanup (std::chrono::system_clock::now ());
	ASSERT_EQ (0, node1.network->size ());

	std::weak_ptr<nano::node> node_w = node1.shared ();
	node1.network->tcp_channels->start_tcp (node2->network->endpoint ());

	ASSERT_TIMELY_EQ (3s, node1.network->size (), 1);
	node1.network->cleanup (test_start);
	ASSERT_EQ (1, node1.network->size ());

	node1.network->cleanup (std::chrono::system_clock::now ());
	ASSERT_TIMELY_EQ (5s, 0, node1.network->size ());
}

TEST (network, loopback_channel)
{
	nano::test::system system (2);
	auto & node1 = *system.nodes[0];
	auto & node2 = *system.nodes[1];
	nano::transport::inproc::channel channel1 (node1, node1);
	ASSERT_EQ (channel1.get_type (), nano::transport::transport_type::loopback);
	ASSERT_EQ (channel1.get_remote_endpoint (), node1.network->endpoint ());
	ASSERT_EQ (channel1.get_tcp_remote_endpoint (), nano::transport::map_endpoint_to_tcp (node1.network->endpoint ()));
	ASSERT_EQ (channel1.get_network_version (), node1.network_params.network.protocol_version);
	ASSERT_EQ (channel1.get_node_id (), node1.node_id.pub);
	ASSERT_EQ (channel1.get_node_id_optional ().value_or (0), node1.node_id.pub);
	nano::transport::inproc::channel channel2 (node2, node2);
	++node1.network->port;
	ASSERT_NE (channel1.get_remote_endpoint (), node1.network->endpoint ());
}

// Ensure the network filters messages with the incorrect magic number
// Disabled, because there is currently no way to send messages with a given network id
TEST (DISABLED_network, filter_invalid_network_bytes)
{
	nano::test::system system{ 2 };
	auto & node1 = *system.nodes[0];
	auto & node2 = *system.nodes[1];

	// find the comms channel that goes from node2 to node1
	auto channel = node2.network->find_node_id (node1.get_node_id ());
	ASSERT_NE (nullptr, channel);

	// send a keepalive, from node2 to node1, with the wrong network bytes
	auto network{ nano::dev::network_params.network };
	network.current_network = nano::networks::invalid;
	nano::keepalive keepalive{ network };
	channel->send (keepalive);

	ASSERT_TIMELY_EQ (5s, 1, node1.stats->count (nano::stat::type::error, nano::stat::detail::invalid_network));
}

// Ensure the network filters messages with the incorrect minimum version
// Disabled, because there is currently no way to send messages with a given version
TEST (DISABLED_network, filter_invalid_version_using)
{
	nano::test::system system{ 2 };
	auto & node1 = *system.nodes[0];
	auto & node2 = *system.nodes[1];

	// find the comms channel that goes from node2 to node1
	auto channel = node2.network->find_node_id (node1.get_node_id ());
	ASSERT_NE (nullptr, channel);

	// send a keepalive, from node2 to node1, with the wrong version_using
	auto network{ nano::dev::network_params.network };
	network.protocol_version = network.protocol_version_min - 1;
	nano::keepalive keepalive{ network };
	channel->send (keepalive);

	ASSERT_TIMELY_EQ (5s, 1, node1.stats->count (nano::stat::type::error, nano::stat::detail::outdated_version));
}

/*
 * Tests that channel and channel container removes channels with dead local sockets
 */
TEST (network, purge_dead_channel_outgoing)
{
	nano::test::system system{};

	nano::node_flags flags;
	// Disable non realtime sockets
	flags.set_disable_bootstrap_bulk_push_client (true);
	flags.set_disable_bootstrap_bulk_pull_server (true);
	flags.set_disable_bootstrap_listener (true);
	flags.set_disable_lazy_bootstrap (true);
	flags.set_disable_legacy_bootstrap (true);
	flags.set_disable_wallet_bootstrap (true);

	auto & node1 = *system.add_node (flags);

	// We expect one incoming and one outgoing connection
	std::shared_ptr<nano::transport::socket> outgoing;
	std::shared_ptr<nano::transport::socket> incoming;

	std::atomic<int> connected_count{ 0 };
	node1.observers->socket_connected.add ([&] (std::shared_ptr<nano::transport::socket> socket) {
		connected_count++;
		outgoing = socket;

		std::cout << "connected: " << outgoing->remote_endpoint () << std::endl;
	});

	std::atomic<int> accepted_count{ 0 };
	node1.observers->socket_accepted.add ([&] (nano::transport::socket & socket) {
		accepted_count++;
		incoming = socket.shared_from_this ();

		std::cout << "accepted: " << socket.remote_endpoint () << std::endl;
	});

	auto & node2 = *system.add_node (flags);

	ASSERT_TIMELY_EQ (5s, connected_count, 1);
	ASSERT_ALWAYS_EQ (1s, connected_count, 1);

	ASSERT_TIMELY_EQ (5s, accepted_count, 1);
	ASSERT_ALWAYS_EQ (1s, accepted_count, 1);

	ASSERT_EQ (node1.network->size (), 1);
	ASSERT_ALWAYS_EQ (1s, node1.network->size (), 1);

	// Store reference to the only channel
	auto channels = node1.network->tcp_channels->list ();
	ASSERT_EQ (channels.size (), 1);
	auto channel = channels.front ();
	ASSERT_TRUE (channel);

	// When socket is dead ensure channel knows about that
	ASSERT_TRUE (channel->alive ());
	outgoing->close ();
	ASSERT_TIMELY (5s, !channel->alive ());

	// Shortly after that a new channel should be established
	ASSERT_TIMELY_EQ (5s, connected_count, 2);
	ASSERT_ALWAYS_EQ (1s, connected_count, 2);

	// Check that a new channel is healthy
	auto channels2 = node1.network->tcp_channels->list ();
	ASSERT_EQ (channels2.size (), 1);
	auto channel2 = channels2.front ();
	ASSERT_TRUE (channel2);
	ASSERT_TRUE (channel2->alive ());
}

/*
 * Tests that channel and channel container removes channels with dead remote sockets
 */
TEST (network, purge_dead_channel_incoming)
{
	nano::test::system system{};

	nano::node_flags flags;
	// Disable non realtime sockets
	flags.set_disable_bootstrap_bulk_push_client (true);
	flags.set_disable_bootstrap_bulk_pull_server (true);
	flags.set_disable_bootstrap_listener (true);
	flags.set_disable_lazy_bootstrap (true);
	flags.set_disable_legacy_bootstrap (true);
	flags.set_disable_wallet_bootstrap (true);

	auto & node1 = *system.add_node (flags);

	// We expect one incoming and one outgoing connection
	std::shared_ptr<nano::transport::socket> outgoing;
	std::shared_ptr<nano::transport::socket> incoming;

	std::atomic<int> connected_count{ 0 };
	node1.observers->socket_connected.add ([&] (std::shared_ptr<nano::transport::socket> socket) {
		connected_count++;
		outgoing = socket;

		std::cout << "connected: " << outgoing->remote_endpoint () << std::endl;
	});

	std::atomic<int> accepted_count{ 0 };
	node1.observers->socket_accepted.add ([&] (nano::transport::socket & socket) {
		accepted_count++;
		incoming = socket.shared_from_this ();

		std::cout << "accepted: " << socket.remote_endpoint () << std::endl;
	});

	auto & node2 = *system.add_node (flags);

	ASSERT_TIMELY_EQ (5s, connected_count, 1);
	ASSERT_ALWAYS_EQ (1s, connected_count, 1);

	ASSERT_TIMELY_EQ (5s, accepted_count, 1);
	ASSERT_ALWAYS_EQ (1s, accepted_count, 1);

	ASSERT_EQ (node2.network->size (), 1);
	ASSERT_ALWAYS_EQ (1s, node2.network->size (), 1);

	// Store reference to the only channel
	auto channels = node2.network->tcp_channels->list ();
	ASSERT_EQ (channels.size (), 1);
	auto channel = channels.front ();
	ASSERT_TRUE (channel);

	// When remote socket is dead ensure channel knows about that
	ASSERT_TRUE (channel->alive ());
	incoming->close ();
	ASSERT_TIMELY (5s, !channel->alive ());

	// Shortly after that a new channel should be established
	ASSERT_TIMELY_EQ (5s, accepted_count, 2);
	ASSERT_ALWAYS_EQ (1s, accepted_count, 2);

	// Check that a new channel is healthy
	auto channels2 = node2.network->tcp_channels->list ();
	ASSERT_EQ (channels2.size (), 1);
	auto channel2 = channels2.front ();
	ASSERT_TRUE (channel2);
	ASSERT_TRUE (channel2->alive ());
}
