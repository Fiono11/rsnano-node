#include <nano/store/lmdb/lmdb.hpp>
#include <nano/store/lmdb/pending.hpp>

namespace
{
nano::store::iterator<nano::pending_key, nano::pending_info> to_iterator (rsnano::LmdbIteratorHandle * it_handle)
{
	if (it_handle == nullptr)
	{
		return { nullptr };
	}

	return { std::make_unique<nano::store::lmdb::iterator<nano::pending_key, nano::pending_info>> (it_handle) };
}
}

nano::store::lmdb::pending::pending (rsnano::LmdbPendingStoreHandle * handle_a) :
	handle{ handle_a } {};

nano::store::lmdb::pending::~pending ()
{
	if (handle != nullptr)
		rsnano::rsn_lmdb_pending_store_destroy (handle);
}

namespace
{
rsnano::PendingKeyDto key_to_dto (nano::pending_key const & key)
{
	rsnano::PendingKeyDto dto;
	std::copy (std::begin (key.account.bytes), std::end (key.account.bytes), std::begin (dto.account));
	std::copy (std::begin (key.hash.bytes), std::end (key.hash.bytes), std::begin (dto.hash));
	return dto;
}

rsnano::PendingInfoDto value_to_dto (nano::pending_info const & value)
{
	rsnano::PendingInfoDto dto;
	std::copy (std::begin (value.source.bytes), std::end (value.source.bytes), std::begin (dto.source));
	std::copy (std::begin (value.amount.bytes), std::end (value.amount.bytes), std::begin (dto.amount));
	dto.epoch = static_cast<uint8_t> (value.epoch);
	return dto;
}
}

void nano::store::lmdb::pending::put (nano::store::write_transaction const & transaction, nano::pending_key const & key, nano::pending_info const & pending)
{
	auto key_dto{ key_to_dto (key) };
	auto value_dto{ value_to_dto (pending) };
	rsnano::rsn_lmdb_pending_store_put (handle, transaction.get_rust_handle (), &key_dto, &value_dto);
}

void nano::store::lmdb::pending::del (nano::store::write_transaction const & transaction, nano::pending_key const & key)
{
	auto key_dto{ key_to_dto (key) };
	rsnano::rsn_lmdb_pending_store_del (handle, transaction.get_rust_handle (), &key_dto);
}

std::optional<nano::pending_info> nano::store::lmdb::pending::get (nano::store::transaction const & transaction, nano::pending_key const & key)
{
	auto key_dto{ key_to_dto (key) };
	rsnano::PendingInfoDto value_dto;
	auto result = rsnano::rsn_lmdb_pending_store_get (handle, transaction.get_rust_handle (), &key_dto, &value_dto);
	if (!result)
	{
		nano::pending_info pending;
		std::copy (std::begin (value_dto.source), std::end (value_dto.source), std::begin (pending.source.bytes));
		std::copy (std::begin (value_dto.amount), std::end (value_dto.amount), std::begin (pending.amount.bytes));
		pending.epoch = static_cast<nano::epoch> (value_dto.epoch);
		return pending;
	}
	return std::nullopt;
}

bool nano::store::lmdb::pending::exists (nano::store::transaction const & transaction_a, nano::pending_key const & key_a)
{
	auto key_dto{ key_to_dto (key_a) };
	return rsnano::rsn_lmdb_pending_store_exists (handle, transaction_a.get_rust_handle (), &key_dto);
}

bool nano::store::lmdb::pending::any (nano::store::transaction const & transaction_a, nano::account const & account_a)
{
	return rsnano::rsn_lmdb_pending_store_any (handle, transaction_a.get_rust_handle (), account_a.bytes.data ());
}

nano::store::iterator<nano::pending_key, nano::pending_info> nano::store::lmdb::pending::begin (nano::store::transaction const & transaction_a, nano::pending_key const & key_a) const
{
	auto key_dto{ key_to_dto (key_a) };
	auto it_handle{ rsnano::rsn_lmdb_pending_store_begin_at_key (handle, transaction_a.get_rust_handle (), &key_dto) };
	return to_iterator (it_handle);
}

nano::store::iterator<nano::pending_key, nano::pending_info> nano::store::lmdb::pending::begin (nano::store::transaction const & transaction_a) const
{
	auto it_handle{ rsnano::rsn_lmdb_pending_store_begin (handle, transaction_a.get_rust_handle ()) };
	return to_iterator (it_handle);
}

nano::store::iterator<nano::pending_key, nano::pending_info> nano::store::lmdb::pending::end () const
{
	return nano::store::iterator<nano::pending_key, nano::pending_info> (nullptr);
}
