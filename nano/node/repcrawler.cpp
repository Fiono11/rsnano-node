#include "nano/lib/rsnano.hpp"
#include "nano/lib/rsnanoutils.hpp"
#include "nano/node/transport/tcp.hpp"

#include <nano/node/node.hpp>
#include <nano/node/repcrawler.hpp>
#include <nano/secure/ledger.hpp>

#include <boost/format.hpp>

#include <chrono>
#include <memory>
#include <stdexcept>

nano::representative::representative (rsnano::RepresentativeHandle * handle_a) :
	handle{ handle_a }
{
}

nano::representative::representative (representative const & other_a) :
	handle{ rsnano::rsn_representative_clone (other_a.handle) }
{
}

nano::representative::~representative ()
{
	rsnano::rsn_representative_destroy (handle);
}

nano::representative & nano::representative::operator= (nano::representative const & other_a)
{
	rsnano::rsn_representative_destroy (handle);
	handle = rsnano::rsn_representative_clone (other_a.handle);
	return *this;
}

nano::account nano::representative::get_account () const
{
	nano::account account;
	rsnano::rsn_representative_account (handle, account.bytes.data ());
	return account;
}

size_t nano::representative::channel_id () const
{
	return rsnano::rsn_representative_channel_id (handle);
}

//------------------------------------------------------------------------------
// representative_register
//------------------------------------------------------------------------------

nano::representative_register::representative_register (rsnano::RepresentativeRegisterHandle * handle) :
	handle{ handle }
{
}

nano::representative_register::~representative_register ()
{
	rsnano::rsn_representative_register_destroy (handle);
}

nano::uint128_t nano::representative_register::total_weight () const
{
	nano::amount result;
	rsnano::rsn_representative_register_total_weight (handle, result.bytes.data ());
	return result.number ();
}

std::vector<nano::representative> nano::representative_register::representatives (std::size_t count, nano::uint128_t const minimum_weight)
{
	nano::amount weight{ minimum_weight };

	auto result_handle = rsnano::rsn_representative_register_representatives (handle, count, weight.bytes.data ());

	auto len = rsnano::rsn_representative_list_len (result_handle);
	std::vector<nano::representative> result;
	result.reserve (len);
	for (auto i = 0; i < len; ++i)
	{
		result.emplace_back (rsnano::rsn_representative_list_get (result_handle, i));
	}
	rsnano::rsn_representative_list_destroy (result_handle);
	return result;
}

/** Total number of representatives */
std::size_t nano::representative_register::representative_count ()
{
	return rsnano::rsn_representative_register_count (handle);
}
//
//------------------------------------------------------------------------------
// rep_crawler
//------------------------------------------------------------------------------

nano::rep_crawler::rep_crawler (rsnano::RepCrawlerHandle * handle, nano::node & node_a) :
	handle{ handle },
	node{ node_a }
{
}

nano::rep_crawler::~rep_crawler ()
{
	rsnano::rsn_rep_crawler_destroy (handle);
}

std::size_t nano::rep_crawler::representative_count ()
{
	return node.representative_register.representative_count ();
}

/*
 * rep_crawler_config
 */

nano::rep_crawler_config::rep_crawler_config (std::chrono::milliseconds query_timeout_a) :
	query_timeout{ query_timeout_a }
{
}

nano::error nano::rep_crawler_config::deserialize (nano::tomlconfig & toml)
{
	auto query_timeout_l = query_timeout.count ();
	toml.get ("query_timeout", query_timeout_l);
	query_timeout = std::chrono::milliseconds{ query_timeout_l };

	return toml.get_error ();
}
