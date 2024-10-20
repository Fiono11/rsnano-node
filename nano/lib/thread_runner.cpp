#include <nano/lib/thread_runner.hpp>
#include <nano/lib/timer.hpp>

#include <boost/format.hpp>

#include <iostream>
#include <thread>

/*
 * thread_runner
 */

nano::thread_runner::thread_runner (boost::asio::io_context & io_ctx_a, unsigned num_threads, const nano::thread_role::name thread_role_a) :
	io_ctx{ io_ctx_a },
	io_guard{ boost::asio::make_work_guard (io_ctx_a) },
	role{ thread_role_a }
{
	for (auto i (0u); i < num_threads; ++i)
	{
		threads.emplace_back (nano::thread_attributes::get_default (), [this, &io_ctx_a, i] () {
			nano::thread_role::set (role);
			try
			{
				logger.debug (nano::log::type::thread_runner, "Thread #{} ({}) started", i, get_string (role));
				run ();
				logger.debug (nano::log::type::thread_runner, "Thread #{} ({}) stopped", i, get_string (role));
			}
			catch (std::exception const & ex)
			{
				std::cerr << ex.what () << std::endl;
#ifndef NDEBUG
				throw; // Re-throw to debugger in debug mode
#endif
			}
			catch (...)
			{
#ifndef NDEBUG
				throw; // Re-throw to debugger in debug mode
#endif
			}
		});
	}
}

nano::thread_runner::~thread_runner ()
{
	join ();
}

void nano::thread_runner::run ()
{
	if constexpr (nano::asio_handler_tracking_threshold () == 0)
	{
		io_ctx.run ();
	}
	else
	{
		nano::timer<> timer;
		timer.start ();
		while (true)
		{
			timer.restart ();
			// Run at most 1 completion handler and record the time it took to complete (non-blocking)
			auto count = io_ctx.poll_one ();
			if (count == 1 && timer.since_start ().count () >= nano::asio_handler_tracking_threshold ())
			{
				logger.warn (nano::log::type::system, "Async handler processing took too long: {}ms", timer.since_start ().count ());
			}
			// Sleep for a bit to give more time slices to other threads
			std::this_thread::sleep_for (std::chrono::milliseconds (5));
			std::this_thread::yield ();
		}
	}
}

void nano::thread_runner::join ()
{
	io_guard.reset ();
	for (auto & i : threads)
	{
		if (i.joinable ())
		{
			i.join ();
		}
	}
}

void nano::thread_runner::stop_event_processing ()
{
	io_guard.get_executor ().context ().stop ();
}
