#pragma once

#include "nano/lib/rsnano.hpp"

#include <nano/lib/relaxed_atomic.hpp>
#include <nano/lib/thread_roles.hpp>
#include <nano/lib/threading.hpp>

#include <chrono>
#include <functional>

namespace nano
{
class thread_pool final
{
public:
	explicit thread_pool (unsigned, nano::thread_role::name);
	explicit thread_pool (rsnano::ThreadPoolHandle * handle);
	thread_pool (thread_pool const &) = delete;
	~thread_pool ();

	/** This will run when there is an available thread for execution */
	void push_task (std::function<void ()>);

	/** Run a task at a certain point in time */
	void add_timed_task (std::chrono::steady_clock::time_point const & expiry_time, std::function<void ()> task);

	/** Stops any further pushed tasks from executing */
	void stop ();

	rsnano::ThreadPoolHandle * handle;
};
} // namespace nano
