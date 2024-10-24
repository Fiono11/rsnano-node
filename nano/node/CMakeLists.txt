add_library(
  node
  ${platform_sources}
  active_elections.hpp
  active_elections.cpp
  backlog_population.hpp
  backlog_population.cpp
  blockprocessor.hpp
  blockprocessor.cpp
  bootstrap/bootstrap_bulk_pull.hpp
  bootstrap/bootstrap_bulk_pull.cpp
  bootstrap/bootstrap_config.hpp
  bootstrap/bootstrap_config.cpp
  bootstrap/bootstrap_connections.hpp
  bootstrap/bootstrap_connections.cpp
  bootstrap/bootstrap.hpp
  bootstrap/bootstrap.cpp
  bootstrap/bootstrap_server.hpp
  bootstrap/bootstrap_server.cpp
  cli.hpp
  cli.cpp
  common.hpp
  common.cpp
  confirming_set.hpp
  confirming_set.cpp
  daemonconfig.hpp
  daemonconfig.cpp
  distributed_work_factory.hpp
  distributed_work_factory.cpp
  election.hpp
  election.cpp
  election_behavior.hpp
  election_insertion_result.hpp
  election_status.hpp
  election_status.cpp
  ipc/action_handler.hpp
  ipc/action_handler.cpp
  ipc/flatbuffers_handler.hpp
  ipc/flatbuffers_handler.cpp
  ipc/flatbuffers_util.hpp
  ipc/flatbuffers_util.cpp
  ipc/ipc_access_config.hpp
  ipc/ipc_access_config.cpp
  ipc/ipc_broker.hpp
  ipc/ipc_broker.cpp
  ipc/ipc_config.hpp
  ipc/ipc_config.cpp
  ipc/ipc_server.hpp
  ipc/ipc_server.cpp
  json_handler.hpp
  json_handler.cpp
  local_vote_history.cpp
  local_vote_history.hpp
  make_store.hpp
  make_store.cpp
  network.hpp
  network.cpp
  nodeconfig.hpp
  nodeconfig.cpp
  node_observers.hpp
  node_observers.cpp
  node_rpc_config.hpp
  node_rpc_config.cpp
  node_wrapper.hpp
  node_wrapper.cpp
  inactive_node.hpp
  inactive_node.cpp
  node.hpp
  node.cpp
  openclconfig.hpp
  openclconfig.cpp
  repcrawler.hpp
  repcrawler.cpp
  rep_tiers.hpp
  rep_tiers.cpp
  request_aggregator.hpp
  request_aggregator.cpp
  rsnano_callbacks.hpp
  rsnano_callbacks.cpp
  scheduler/component.hpp
  scheduler/component.cpp
  scheduler/hinted.hpp
  scheduler/hinted.cpp
  scheduler/manual.hpp
  scheduler/manual.cpp
  scheduler/optimistic.hpp
  scheduler/optimistic.cpp
  scheduler/priority.hpp
  scheduler/priority.cpp
  telemetry.hpp
  telemetry.cpp
  transport/tcp.hpp
  transport/tcp.cpp
  transport/tcp_listener.hpp
  transport/tcp_listener.cpp
  transport/transport.hpp
  transport/transport.cpp
  unchecked_map.cpp
  unchecked_map.hpp
  vote_cache.hpp
  vote_cache.cpp
  vote_processor.hpp
  vote_processor.cpp
  vote_spacing.hpp
  vote_spacing.cpp
  vote_with_weight_info.hpp
  wallet.hpp
  wallet.cpp
  websocket.hpp
  websocket.cpp
  websocketconfig.hpp
  websocketconfig.cpp
  messages.hpp
  messages.cpp)

target_link_libraries(
  node
  nano_lib
  nano_store
  secure
  rpc
  Boost::beast
  Boost::log_setup
  Boost::log
  Boost::program_options
  Boost::stacktrace
  Boost::system
  Boost::thread
  OpenSSL::SSL
  ${CMAKE_DL_LIBS}
  ${psapi_lib})

target_compile_definitions(
  node PRIVATE -DTAG_VERSION_STRING=${TAG_VERSION_STRING}
               -DGIT_COMMIT_HASH=${GIT_COMMIT_HASH})

# This ensures that any changes to Flatbuffers source files will cause a
# regeneration of any C++ header files.
add_dependencies(node ipc_flatbuffers_lib)

include_directories(${CMAKE_SOURCE_DIR}/submodules)
include_directories(
  ${CMAKE_SOURCE_DIR}/submodules/nano-pow-server/deps/cpptoml/include)
