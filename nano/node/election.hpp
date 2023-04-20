#pragma once

#include <nano/secure/common.hpp>
#include <nano/secure/ledger.hpp>
#include <nano/secure/store.hpp>

#include <atomic>
#include <chrono>
#include <memory>
#include <unordered_set>

namespace nano
{
class channel;
class confirmation_solicitor;
class inactive_cache_information;
class node;

class vote_info final
{
public:
	std::chrono::steady_clock::time_point time;
	uint64_t timestamp;
	nano::block_hash hash;
	nano::vote_type type;
};

class vote_with_weight_info final
{
public:
	nano::account representative;
	std::chrono::steady_clock::time_point time;
	uint64_t timestamp;
	nano::block_hash hash;
	nano::uint128_t weight;
	nano::vote_type type;
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

class election final : public std::enable_shared_from_this<nano::election>
{
public:
	enum class vote_source
	{
		live,
		cache,
	};

private:
	// Minimum time between broadcasts of the current winner of an election, as a backup to requesting confirmations
	std::chrono::milliseconds base_latency () const;
	std::function<void (std::shared_ptr<nano::block> const &)> confirmation_action;
	std::function<void (nano::account const &)> live_vote_action;

private: // State management
	enum class state_t
	{
		passive, // only listening for incoming votes
		active, // actively request confirmations
		confirmed, // confirmed but still listening for votes
		expired_confirmed,
		expired_unconfirmed
	};

	static unsigned constexpr passive_duration_factor = 5;
	static unsigned constexpr active_request_count_min = 2;
	std::atomic<nano::election::state_t> state_m = { state_t::passive };

	static_assert (std::is_trivial<std::chrono::steady_clock::duration> ());
	std::atomic<std::chrono::steady_clock::duration> state_start{ std::chrono::steady_clock::now ().time_since_epoch () };

	// These are modified while not holding the mutex from transition_time only
	std::chrono::steady_clock::time_point last_block = { std::chrono::steady_clock::now () };
	std::chrono::steady_clock::time_point last_req = {};
	/** The last time vote for this election was generated */
	std::chrono::steady_clock::time_point last_vote = {};

	bool valid_change (nano::election::state_t, nano::election::state_t) const;
	bool state_change (nano::election::state_t, nano::election::state_t);

public: // State transitions
	bool transition_time (nano::confirmation_solicitor &);
	void transition_active ();

public: // Status
	// Returns true when the election is confirmed in memory
	// Elections will first confirm in memory once sufficient votes have been received
	bool status_confirmed () const;
	// Returns true when the winning block is durably confirmed in the ledger.
	// Later once the confirmation height processor has updated the confirmation height it will be confirmed on disk
	// It is possible for an election to be confirmed on disk but not in memory, for instance if implicitly confirmed via confirmation height
	bool confirmed () const;
	bool failed () const;
	nano::election_extended_status current_status () const;
	std::shared_ptr<nano::block> winner () const;
	std::atomic<unsigned> confirmation_request_count{ 0 };

	void log_votes (nano::tally_t const &, std::string const & = "") const;
	nano::tally_t tally () const;
	bool have_quorum (nano::tally_t const &) const;

	// Guarded by mutex
	nano::election_status status;

public: // Interface
	election (nano::node &, std::shared_ptr<nano::block> const & block, std::function<void (std::shared_ptr<nano::block> const &)> const & confirmation_action, std::function<void (nano::account const &)> const & vote_action, nano::election_behavior behavior);

	std::shared_ptr<nano::block> find (nano::block_hash const &) const;
	/*
	 * Process vote. Internally uses cooldown to throttle non-final votes
	 * If the election reaches consensus, it will be confirmed
	 */
	nano::election_vote_result vote (nano::account const & representative, uint64_t timestamp, nano::block_hash const & block_hash, vote_source = vote_source::live, nano::vote_type type = nano::vote_type::vote, uint8_t round = 0);
	bool publish (std::shared_ptr<nano::block> const & block_a);
	// Confirm this block if quorum is met
	void confirm_if_quorum (nano::unique_lock<nano::mutex> &);

	/**
	 * Broadcasts vote for the current winner of this election
	 * Checks if sufficient amount of time (`vote_generation_interval`) passed since the last vote generation
	 */
	void broadcast_vote ();

private: // Dependencies
	nano::node & node;

public: // Information
	uint64_t const height;
	nano::root const root;
	nano::qualified_root const qualified_root;
	std::vector<nano::vote_with_weight_info> votes_with_weight () const;
	nano::election_behavior behavior () const;

private:
	nano::tally_t tally_impl () const;
	// lock_a does not own the mutex on return
	void confirm_once (nano::unique_lock<nano::mutex> & lock_a, nano::election_status_type = nano::election_status_type::active_confirmed_quorum);
	void broadcast_block (nano::confirmation_solicitor &);
	void send_confirm_req (nano::confirmation_solicitor &);
	/**
	 * Broadcast vote for current election winner. Generates final vote if reached quorum or already confirmed
	 * Requires mutex lock
	 */
	void broadcast_vote_impl ();
	void remove_votes (nano::block_hash const &);
	void remove_block (nano::block_hash const &);
	bool replace_by_weight (nano::unique_lock<nano::mutex> & lock_a, nano::block_hash const &);
	std::chrono::milliseconds time_to_live () const;
	/**
	 * Calculates minimum time delay between subsequent votes when processing non-final votes
	 */
	std::chrono::seconds cooldown_time (nano::uint128_t weight) const;
	/**
	 * Calculates time delay between broadcasting confirmation requests
	 */
	std::chrono::milliseconds confirm_req_time () const;

private:
	std::unordered_map<nano::block_hash, std::shared_ptr<nano::block>> last_blocks;
	std::unordered_map<nano::account, nano::vote_info> last_votes;
	std::atomic<bool> is_quorum{ false };
	mutable nano::uint128_t final_weight{ 0 };
	mutable std::unordered_map<nano::block_hash, nano::uint128_t> last_tally;

	nano::election_behavior const behavior_m{ nano::election_behavior::normal };
	std::chrono::steady_clock::time_point const election_start = { std::chrono::steady_clock::now () };

	mutable nano::mutex mutex;

	uint64_t current_round{ 0 };
	bool voted_in_current_round{ false };

private: // Constants
	static std::size_t constexpr max_blocks{ 10 };

	friend class active_transactions;
	friend class confirmation_solicitor;

public: // Only used in tests
	void force_confirm (nano::election_status_type = nano::election_status_type::active_confirmed_quorum);
	std::unordered_map<nano::account, nano::vote_info> votes () const;
	std::unordered_map<nano::block_hash, std::shared_ptr<nano::block>> blocks () const;

	friend class confirmation_solicitor_different_hash_Test;
	friend class confirmation_solicitor_bypass_max_requests_cap_Test;
	friend class votes_add_existing_Test;
	friend class votes_add_old_Test;
};
}
