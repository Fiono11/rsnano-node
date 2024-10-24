#pragma once

#include <nano/store/peer.hpp>

namespace nano::store::lmdb
{
class peer : public nano::store::peer
{
private:
	rsnano::LmdbPeerStoreHandle * handle;

public:
	explicit peer (rsnano::LmdbPeerStoreHandle * handle_a);
	~peer ();
	peer (peer const &) = delete;
	peer (peer &&) = delete;
	size_t count (nano::store::transaction const & transaction_a) const override;
	void clear (nano::store::write_transaction const & transaction_a) override;
};
}
