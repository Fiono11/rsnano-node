#pragma once

#include "nano/lib/rsnano.hpp"

#include <nano/lib/locks.hpp>
#include <nano/lib/numbers.hpp>
#include <nano/lib/observer_set.hpp>
#include <nano/secure/common.hpp>

namespace nano::store
{
class component;
class transaction;
}
namespace nano
{
class account_info;
class ledger;
class election_scheduler;
class stats;

class backlog_population final
{
public:
	backlog_population (rsnano::BacklogPopulationHandle * handle);
	backlog_population (backlog_population const &) = delete;
	backlog_population (backlog_population &&) = delete;
	~backlog_population ();

	/** Manually trigger backlog population */
	void trigger ();

	void set_activate_callback (std::function<void (nano::store::transaction const &, nano::account const &)>);

private:
	rsnano::BacklogPopulationHandle * handle;
};
}
