#include "nano/lib/numbers.hpp"
#include "nano/lib/rsnano.hpp"
#include <nano/node/gap_cache.hpp>
#include <nano/node/node.hpp>
#include <nano/secure/store.hpp>

#include <boost/format.hpp>
#include <_types/_uint8_t.h>

namespace
{
class gap_cache_bootstrap_starter
{
public:
	gap_cache_bootstrap_starter (nano::node & node_a) :
		node{ node_a }
	{
	}

	void bootstrap_start (nano::block_hash const & hash_a)
	{
		auto node_l (node.shared ());
		node.workers->add_timed_task (std::chrono::steady_clock::now () + node.network_params.bootstrap.gap_cache_bootstrap_start_interval, [node_l, hash_a] () {
			if (!node_l->ledger.block_or_pruned_exists (hash_a))
			{
				if (!node_l->bootstrap_initiator.in_progress ())
				{
					node_l->logger->try_log (boost::str (boost::format ("Missing block %1% which has enough votes to warrant lazy bootstrapping it") % hash_a.to_string ()));
				}
				if (!node_l->flags.disable_lazy_bootstrap ())
				{
					node_l->bootstrap_initiator.bootstrap_lazy (hash_a);
				}
				else if (!node_l->flags.disable_legacy_bootstrap ())
				{
					node_l->bootstrap_initiator.bootstrap ();
				}
			}
		});
	}

private:
	nano::node & node;
};

void start_bootstrap_callback_wrapper (void * context, const uint8_t * bytes)
{
	auto fn = static_cast<std::function<void (nano::block_hash const &)> *> (context);
	nano::block_hash hash;
	hash = nano::block_hash::from_bytes(bytes);
	(*fn) (hash);
}

void drop_start_bootstrap_callback (void * context_a)
{
	auto fn = static_cast<std::function<void (nano::block_hash const &)> *> (context_a);
	delete fn;
}
}

nano::gap_cache::gap_cache (nano::node & node_a) :
	node (node_a)
	//handle{ rsnano::rsn_gap_cache_create (node.ledger.get_handle()) }
{
	gap_cache_bootstrap_starter bootstrap_starter{ node_a };
	start_bootstrap_callback = [bootstrap_starter] (nano::block_hash const & hash_a) mutable {
		bootstrap_starter.bootstrap_start (hash_a);
	};
	auto context = new std::function<void (nano::block_hash const &)> (start_bootstrap_callback);

	handle = rsnano::rsn_gap_cache_create(
		node.config->to_dto(), 
		node.online_reps.get_handle(), 
		node.ledger.get_handle(),
		node.flags.handle,
		start_bootstrap_callback_wrapper,
		context,
		drop_start_bootstrap_callback
	);

	//auto ledger = dynamic_cast<nano::ledger *> (&node.ledger);
	//auto online_reps = dynamic_cast<nano::online_reps *> (&node.online_reps);
}

nano::gap_cache::~gap_cache ()
{
	rsnano::rsn_gap_cache_destroy (handle);
}

void nano::gap_cache::add (nano::block_hash const & hash_a, std::chrono::steady_clock::time_point time_point_a)
{
	rsnano::rsn_gap_cache_add(handle, hash_a.bytes.data(), time_point_a.time_since_epoch ().count ());
	/*nano::lock_guard<nano::mutex> lock{ mutex };
	auto existing (blocks.get<tag_hash> ().find (hash_a));
	if (existing != blocks.get<tag_hash> ().end ())
	{
		blocks.get<tag_hash> ().modify (existing, [time_point_a] (nano::gap_information & info) {
			info.arrival = time_point_a;
		});
	}
	else
	{
		blocks.get<tag_arrival> ().emplace (nano::gap_information{ time_point_a, hash_a, std::vector<nano::account> () });
		if (blocks.get<tag_arrival> ().size () > max)
		{
			blocks.get<tag_arrival> ().erase (blocks.get<tag_arrival> ().begin ());
		}
	}*/
}

void nano::gap_cache::erase (nano::block_hash const & hash_a)
{
	rsnano::rsn_gap_cache_erase(handle, hash_a.bytes.data());
	//nano::lock_guard<nano::mutex> lock{ mutex };
	//blocks.get<tag_hash> ().erase (hash_a);
}

void nano::gap_cache::vote (std::shared_ptr<nano::vote> const & vote_a)
{
	rsnano::rsn_gap_cache_vote(handle, vote_a->get_handle());
	/*nano::lock_guard<nano::mutex> lock{ mutex };
	for (auto const & hash : vote_a->hashes ())
	{
		auto & gap_blocks_by_hash (blocks.get<tag_hash> ());
		auto existing (gap_blocks_by_hash.find (hash));
		if (existing != gap_blocks_by_hash.end () && !existing->bootstrap_started)
		{
			auto is_new (false);
			gap_blocks_by_hash.modify (existing, [&is_new, &vote_a] (nano::gap_information & info) {
				auto it = std::find (info.voters.begin (), info.voters.end (), vote_a->account ());
				is_new = (it == info.voters.end ());
				if (is_new)
				{
					info.voters.push_back (vote_a->account ());
				}
			});

			if (is_new)
			{
				if (bootstrap_check (existing->voters, hash))
				{
					gap_blocks_by_hash.modify (existing, [] (nano::gap_information & info) {
						info.bootstrap_started = true;
					});
				}
			}
		}
	}*/
}

bool nano::gap_cache::bootstrap_check (std::vector<nano::account> const & voters_a, nano::block_hash const & hash_a)
{
	std::vector<uint8_t> bytes(voters_a.size() * sizeof(nano::account));
	const auto* voters_ptr = voters_a.data();

	for (size_t i = 0; i < voters_a.size(); i++) {
		const auto* voter_bytes = reinterpret_cast<const uint8_t*>(&voters_ptr[i]);
		std::copy(voter_bytes, voter_bytes + sizeof(nano::account), bytes.data() + (i * sizeof(nano::account)));
	}

	const size_t voters_bytes_size = voters_a.size() * sizeof(nano::account);
	const uint8_t* voters_bytes_ptr = bytes.data();

	rsnano::rsn_gap_cache_bootstrap_check(handle, voters_bytes_size, voters_bytes_ptr, hash_a.bytes.data());

	/*nano::uint128_t tally;
	for (auto const & voter : voters_a)
	{
		tally += node.ledger.weight (voter);
	}
	bool start_bootstrap (false);
	if (!node.flags.disable_lazy_bootstrap ())
	{
		if (tally >= node.online_reps.delta ())
		{
			start_bootstrap = true;
		}
	}
	else if (!node.flags.disable_legacy_bootstrap () && tally > bootstrap_threshold ())
	{
		start_bootstrap = true;
	}
	if (start_bootstrap && !node.ledger.block_or_pruned_exists (hash_a))
	{
		bootstrap_start (hash_a);
	}
	return start_bootstrap;*/
	return true;
}

void nano::gap_cache::bootstrap_start (nano::block_hash const & hash_a)
{
	start_bootstrap_callback (hash_a);
}

nano::uint128_t nano::gap_cache::bootstrap_threshold ()
{
	//auto result ((node.online_reps.trended () / 256) * node.config->bootstrap_fraction_numerator);
	//return result;
	nano::amount size;
	rsnano::rsn_gap_cache_bootstrap_threshold (handle, size.bytes.data ());
	return size.number ();
}

std::size_t nano::gap_cache::size ()
{
	//nano::lock_guard<nano::mutex> lock{ mutex };
	//return blocks.size ();
	return rsnano::rsn_gap_cache_size(handle);
}

bool nano::gap_cache::block_exists (nano::block_hash const & hash_a)
{
	return rsnano::rsn_gap_cache_block_exists(handle, hash_a.bytes.data());
}

std::chrono::steady_clock::time_point nano::gap_cache::earliest ()
{	
	auto value = rsnano::rsn_gap_cache_earliest (handle);
	return std::chrono::steady_clock::time_point (std::chrono::steady_clock::duration (value));
}

std::chrono::steady_clock::time_point nano::gap_cache::block_arrival (nano::block_hash const & hash_a)
{	
	auto value = rsnano::rsn_gap_cache_block_arrival (handle, hash_a.bytes.data());
	return std::chrono::steady_clock::time_point (std::chrono::steady_clock::duration (value));
}

std::unique_ptr<nano::container_info_component> nano::collect_container_info (gap_cache & gap_cache, std::string const & name)
{
	auto count = gap_cache.size ();
	auto sizeof_element = sizeof (decltype (gap_cache.blocks)::value_type);
	auto composite = std::make_unique<container_info_composite> (name);
	composite->add_component (std::make_unique<container_info_leaf> (container_info{ "blocks", count, sizeof_element }));
	return composite;
}

