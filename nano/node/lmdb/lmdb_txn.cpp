#include <nano/lib/jsonconfig.hpp>
#include <nano/lib/logger_mt.hpp>
#include <nano/lib/threading.hpp>
#include <nano/lib/utility.hpp>
#include <nano/node/lmdb/lmdb_env.hpp>
#include <nano/node/lmdb/lmdb_txn.hpp>
#include <nano/secure/store.hpp>

#include <boost/format.hpp>

// Some builds (mac) fail due to "Boost.Stacktrace requires `_Unwind_Backtrace` function".
#ifndef _WIN32
#ifdef NANO_STACKTRACE_BACKTRACE
#define BOOST_STACKTRACE_USE_BACKTRACE
#endif
#ifndef _GNU_SOURCE
#define BEFORE_GNU_SOURCE 0
#define _GNU_SOURCE
#else
#define BEFORE_GNU_SOURCE 1
#endif
#endif
// On Windows this include defines min/max macros, so keep below other includes
// to reduce conflicts with other std functions
#include <boost/stacktrace.hpp>
#ifndef _WIN32
#if !BEFORE_GNU_SOURCE
#undef _GNU_SOURCE
#endif
#endif

namespace
{
class matches_txn final
{
public:
	explicit matches_txn (uint64_t txn_id_a) :
		txn_id (txn_id_a)
	{
	}

	bool operator() (nano::mdb_txn_stats const & mdb_txn_stats)
	{
		return mdb_txn_stats.txn_id == txn_id;
	}

private:
	uint64_t txn_id;
};
}

nano::read_mdb_txn::read_mdb_txn (uint64_t txn_id_a, MDB_env * env_a, nano::mdb_txn_callbacks txn_callbacks_a) :
	txn_handle{ rsnano::rsn_lmdb_read_txn_create (txn_id_a, reinterpret_cast<rsnano::MdbEnv *> (env_a), new nano::mdb_txn_callbacks{ txn_callbacks_a }) }
{
}

nano::read_mdb_txn::read_mdb_txn (rsnano::TransactionHandle * handle_a) :
	txn_handle{ handle_a }
{
}

nano::read_mdb_txn::~read_mdb_txn ()
{
	rsnano::rsn_lmdb_read_txn_destroy (txn_handle);
}

void nano::read_mdb_txn::reset ()
{
	rsnano::rsn_lmdb_read_txn_reset (txn_handle);
}

void nano::read_mdb_txn::renew ()
{
	rsnano::rsn_lmdb_read_txn_renew (txn_handle);
}

void nano::read_mdb_txn::refresh ()
{
	rsnano::rsn_lmdb_read_txn_refresh (txn_handle);
}

void * nano::read_mdb_txn::get_handle () const
{
	return rsnano::rsn_lmdb_read_txn_handle (txn_handle);
}

nano::write_mdb_txn::write_mdb_txn (uint64_t txn_id_a, MDB_env * env_a, nano::mdb_txn_callbacks txn_callbacks_a) :
	txn_handle{ rsnano::rsn_lmdb_write_txn_create (txn_id_a, reinterpret_cast<rsnano::MdbEnv *> (env_a), new nano::mdb_txn_callbacks{ txn_callbacks_a }) }
{
}

nano::write_mdb_txn::write_mdb_txn (rsnano::TransactionHandle * handle_a) :
	txn_handle{ handle_a }
{
}

nano::write_mdb_txn::~write_mdb_txn ()
{
	rsnano::rsn_lmdb_write_txn_destroy (txn_handle);
}

void nano::write_mdb_txn::commit ()
{
	rsnano::rsn_lmdb_write_txn_commit (txn_handle);
}

void nano::write_mdb_txn::renew ()
{
	rsnano::rsn_lmdb_write_txn_renew (txn_handle);
}

void nano::write_mdb_txn::refresh ()
{
	rsnano::rsn_lmdb_write_txn_refresh (txn_handle);
}

void * nano::write_mdb_txn::get_handle () const
{
	return rsnano::rsn_lmdb_write_txn_handle (txn_handle);
}

bool nano::write_mdb_txn::contains (nano::tables table_a) const
{
	// LMDB locks on every write
	return true;
}

nano::mdb_txn_tracker::mdb_txn_tracker (nano::logger_mt & logger_a, nano::txn_tracking_config const & txn_tracking_config_a, std::chrono::milliseconds block_processor_batch_max_time_a) :
	logger (logger_a),
	txn_tracking_config (txn_tracking_config_a),
	block_processor_batch_max_time (block_processor_batch_max_time_a)
{
}

void nano::mdb_txn_tracker::serialize_json (boost::property_tree::ptree & json, std::chrono::milliseconds min_read_time, std::chrono::milliseconds min_write_time)
{
	// Copying is cheap compared to generating the stack trace strings, so reduce time holding the mutex
	std::vector<mdb_txn_stats> copy_stats;
	std::vector<bool> are_writes;
	{
		nano::lock_guard<nano::mutex> guard (mutex);
		copy_stats = stats;
		are_writes.reserve (stats.size ());
		std::transform (stats.cbegin (), stats.cend (), std::back_inserter (are_writes), [] (auto & mdb_txn_stat) {
			return mdb_txn_stat.is_write ();
		});
	}

	// Get the time difference now as creating stacktraces (Debug/Windows for instance) can take a while so results won't be as accurate
	std::vector<std::chrono::milliseconds> times_since_start;
	times_since_start.reserve (copy_stats.size ());
	std::transform (copy_stats.cbegin (), copy_stats.cend (), std::back_inserter (times_since_start), [] (auto const & stat) {
		return stat.timer.since_start ();
	});
	debug_assert (times_since_start.size () == copy_stats.size ());

	for (std::size_t i = 0; i < times_since_start.size (); ++i)
	{
		auto const & stat = copy_stats[i];
		auto time_held_open = times_since_start[i];

		if ((are_writes[i] && time_held_open >= min_write_time) || (!are_writes[i] && time_held_open >= min_read_time))
		{
			nano::jsonconfig mdb_lock_config;

			mdb_lock_config.put ("thread", stat.thread_name);
			mdb_lock_config.put ("time_held_open", time_held_open.count ());
			mdb_lock_config.put ("write", !!are_writes[i]);

			boost::property_tree::ptree stacktrace_config;
			for (auto frame : *stat.stacktrace)
			{
				nano::jsonconfig frame_json;
				frame_json.put ("name", frame.name ());
				frame_json.put ("address", frame.address ());
				frame_json.put ("source_file", frame.source_file ());
				frame_json.put ("source_line", frame.source_line ());
				stacktrace_config.push_back (std::make_pair ("", frame_json.get_tree ()));
			}

			nano::jsonconfig stack (stacktrace_config);
			mdb_lock_config.put_child ("stacktrace", stack);
			json.push_back (std::make_pair ("", mdb_lock_config.get_tree ()));
		}
	}
}

void nano::mdb_txn_tracker::log_if_held_long_enough (nano::mdb_txn_stats const & mdb_txn_stats) const
{
	// Only log these transactions if they were held for longer than the min_read_txn_time/min_write_txn_time config values
	auto is_write = mdb_txn_stats.is_write ();
	auto time_open = mdb_txn_stats.timer.since_start ();

	auto should_ignore = false;
	// Reduce noise in log files by removing any entries from the block processor (if enabled) which are less than the max batch time (+ a few second buffer) because these are expected writes during bootstrapping.
	auto is_below_max_time = time_open <= (block_processor_batch_max_time + std::chrono::seconds (3));
	bool is_blk_processing_thread = mdb_txn_stats.thread_name == nano::thread_role::get_string (nano::thread_role::name::block_processing);
	if (txn_tracking_config.ignore_writes_below_block_processor_max_time && is_blk_processing_thread && is_write && is_below_max_time)
	{
		should_ignore = true;
	}

	if (!should_ignore && ((is_write && time_open >= txn_tracking_config.min_write_txn_time) || (!is_write && time_open >= txn_tracking_config.min_read_txn_time)))
	{
		debug_assert (mdb_txn_stats.stacktrace);
		logger.always_log (boost::str (boost::format ("%1%ms %2% held on thread %3%\n%4%") % mdb_txn_stats.timer.since_start ().count () % (is_write ? "write lock" : "read") % mdb_txn_stats.thread_name % *mdb_txn_stats.stacktrace));
	}
}

void nano::mdb_txn_tracker::add (uint64_t txn_id, bool is_write)
{
	nano::lock_guard<nano::mutex> guard (mutex);
	debug_assert (std::find_if (stats.cbegin (), stats.cend (), matches_txn (txn_id)) == stats.cend ());
	stats.emplace_back (txn_id, is_write);
}

/** Can be called without error if transaction does not exist */
void nano::mdb_txn_tracker::erase (uint64_t txn_id)
{
	nano::unique_lock<nano::mutex> lk (mutex);
	auto it = std::find_if (stats.begin (), stats.end (), matches_txn (txn_id));
	if (it != stats.end ())
	{
		auto tracker_stats_copy = *it;
		stats.erase (it);
		lk.unlock ();
		log_if_held_long_enough (tracker_stats_copy);
	}
}

nano::mdb_txn_stats::mdb_txn_stats (uint64_t txn_id_a, bool is_write_a) :
	txn_id{ txn_id_a },
	is_write_m{ is_write_a },
	thread_name (nano::thread_role::get_string ()),
	stacktrace (std::make_shared<boost::stacktrace::stacktrace> ())
{
	timer.start ();
}

bool nano::mdb_txn_stats::is_write () const
{
	return is_write_m;
}
