#pragma once

#include <nano/lib/numbers.hpp>
#include <nano/store/db_val.hpp>

namespace nano
{
class wallet_value
{
public:
	wallet_value () = default;
	wallet_value (nano::store::db_val<rsnano::MdbVal> const &);
	wallet_value (nano::raw_key const &, uint64_t);
	nano::raw_key key;
	uint64_t work;
};
}
