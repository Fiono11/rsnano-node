#include <nano/store/confirmation_height.hpp>

std::optional<nano::confirmation_height_info> nano::store::confirmation_height::get (const nano::store::transaction & transaction, const nano::account & account)
{
	nano::confirmation_height_info info;
	bool error = get (transaction, account, info);
	if (!error)
	{
		return info;
	}
	else
	{
		return std::nullopt;
	}
}
