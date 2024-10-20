#pragma once

#include <nano/lib/errors.hpp>
#include <nano/lib/rsnano.hpp>
#include <nano/node/node_rpc_config.hpp>
#include <nano/node/nodeconfig.hpp>
#include <nano/node/openclconfig.hpp>

#include <string>
#include <vector>

namespace nano
{
class tomlconfig;
class daemon_config
{
public:
	daemon_config () = default;
	daemon_config (std::filesystem::path const & data_path, nano::network_params & network_params);
	nano::error deserialize_toml (nano::tomlconfig &);
	std::string serialize_toml ();
	bool rpc_enable{ false };
	nano::node_rpc_config rpc;
	nano::node_config node;
	bool opencl_enable{ false };
	nano::opencl_config opencl;
	std::filesystem::path data_path;
};

nano::error read_node_config_toml (std::filesystem::path const &, nano::daemon_config & config_a, std::vector<std::string> const & config_overrides = std::vector<std::string> ());
}
