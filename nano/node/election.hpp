#pragma once
#include "nano/lib/rsnano.hpp"

#include <nano/node/vote_cache.hpp>
#include <nano/secure/common.hpp>
#include <nano/store/component.hpp>

#include <chrono>
#include <memory>

namespace nano
{
class channel;
class confirmation_solicitor;
class inactive_cache_information;
class node;

class vote_info final
{
public:
	vote_info () :
		handle{ rsnano::rsn_vote_info_create1 () }
	{
	}

	vote_info (uint64_t timestamp, nano::block_hash hash) :
		handle{ rsnano::rsn_vote_info_create2 (timestamp, hash.bytes.data ()) }
	{
	}

	vote_info (rsnano::VoteInfoHandle * handle) :
		handle{ handle }
	{
	}

	vote_info (vote_info && other) :
		handle{ other.handle }
	{
		other.handle = nullptr;
	}

	vote_info (vote_info const & other) :
		handle{ rsnano::rsn_vote_info_clone (other.handle) }
	{
	}

	~vote_info ()
	{
		if (handle != nullptr)
		{
			rsnano::rsn_vote_info_destroy (handle);
		}
	}

	vote_info & operator= (vote_info const & other)
	{
		if (handle != nullptr)
		{
			rsnano::rsn_vote_info_destroy (handle);
		}
		handle = rsnano::rsn_vote_info_clone (other.handle);
		return *this;
	}

	std::chrono::system_clock::time_point get_time () const
	{
		auto value = rsnano::rsn_vote_info_time_ns (handle);
		return std::chrono::system_clock::time_point (std::chrono::duration_cast<std::chrono::system_clock::duration> (std::chrono::nanoseconds (value)));
	}

	vote_info with_relative_time (std::chrono::seconds seconds)
	{
		return { rsnano::rsn_vote_info_with_relative_time (handle, seconds.count ()) };
	}

	uint64_t get_timestamp () const
	{
		return rsnano::rsn_vote_info_timestamp (handle);
	}

	nano::block_hash get_hash () const
	{
		nano::block_hash hash;
		rsnano::rsn_vote_info_hash (handle, hash.bytes.data ());
		return hash;
	}

	rsnano::VoteInfoHandle * handle;
};

class vote_with_weight_info final
{
public:
	nano::account representative;
	std::chrono::system_clock::time_point time;
	uint64_t timestamp;
	nano::block_hash hash;
	nano::uint128_t weight;
};

class election_vote_result final
{
public:
	election_vote_result () = default;
	election_vote_result (bool, bool);
	bool replay{ false };
	bool processed{ false };
};

enum class election_behavior
{
	normal,
	/**
	 * Hinted elections:
	 * - shorter timespan
	 * - limited space inside AEC
	 */
	hinted,
	/**
	 * Optimistic elections:
	 * - shorter timespan
	 * - limited space inside AEC
	 * - more frequent confirmation requests
	 */
	optimistic,
};

nano::stat::detail to_stat_detail (nano::election_behavior);

struct election_extended_status final
{
	nano::election_status status;
	std::unordered_map<nano::account, nano::vote_info> votes;
	nano::tally_t tally;
};

class election;

enum class election_state
{
	passive, // only listening for incoming votes
	active, // actively request confirmations
	confirmed, // confirmed but still listening for votes
	expired_confirmed,
	expired_unconfirmed
};

class election_lock
{
public:
	election_lock (nano::election const & election);
	election_lock (election_lock const &) = delete;
	~election_lock ();
	void unlock ();
	void lock ();
	nano::election_status status () const;
	void set_status (nano::election_status status);
	bool state_change (nano::election_state expected_a, nano::election_state desired_a);

	void insert_or_assign_last_block (std::shared_ptr<nano::block> const & block);
	void erase_last_block (nano::block_hash const & hash);
	size_t last_blocks_size () const;
	std::unordered_map<nano::block_hash, std::shared_ptr<nano::block>> last_blocks () const;
	std::shared_ptr<nano::block> find_block (nano::block_hash const & hash);

	void insert_or_assign_vote (nano::account const & account, nano::vote_info const & vote_info);
	std::optional<nano::vote_info> find_vote (nano::account const & account) const;
	size_t last_votes_size () const;
	std::unordered_map<nano::account, nano::vote_info> last_votes () const;
	void erase_vote (nano::account const & account);
	void set_final_weight (nano::amount const & weight);
	nano::amount final_weight () const;

	nano::election & election;
	rsnano::ElectionLockHandle * handle;
};

enum class vote_source
{
	live,
	cache,
};

class election final : public std::enable_shared_from_this<nano::election>
{
private: // State management
	static unsigned constexpr passive_duration_factor = 5;
	static unsigned constexpr active_request_count_min = 2;

public: // State transitions
	nano::election_lock lock () const;
	void transition_active ();

public: // Status
	bool failed () const;
	std::shared_ptr<nano::block> winner () const;
	unsigned get_confirmation_request_count () const;
	void inc_confirmation_request_count ();

public: // Interface
	election (nano::node &, std::shared_ptr<nano::block> const & block, std::function<void (std::shared_ptr<nano::block> const &)> const & confirmation_action, std::function<void (nano::account const &)> const & vote_action, nano::election_behavior behavior);
	election (rsnano::ElectionHandle * handle_a);
	election (election const &) = delete;
	election (election &&) = delete;
	~election ();

	std::shared_ptr<nano::block> find (nano::block_hash const &) const;
	void set_status_type (nano::election_status_type status_type);

	nano::vote_info get_last_vote (nano::account const & account);
	void set_last_vote (nano::account const & account, nano::vote_info vote_info);
	nano::election_status get_status () const;
	void set_status (nano::election_status status_a);

public: // Information
	nano::root root () const;
	nano::qualified_root qualified_root () const;
	nano::election_behavior behavior () const;

private:
	std::chrono::milliseconds time_to_live () const;
	bool is_quorum () const;

public: // Logging
	void operator() (nano::object_stream &) const;

private: // Constants
	static std::size_t constexpr max_blocks{ 10 };

	friend class active_transactions;
	friend class confirmation_solicitor;
	friend class election_helper;

public: // Only used in tests
	std::unordered_map<nano::account, nano::vote_info> votes () const;
	std::unordered_map<nano::block_hash, std::shared_ptr<nano::block>> blocks () const;

	friend class confirmation_solicitor_different_hash_Test;
	friend class confirmation_solicitor_bypass_max_requests_cap_Test;
	friend class votes_add_existing_Test;
	friend class votes_add_old_Test;
	rsnano::ElectionHandle * handle;
};
}
