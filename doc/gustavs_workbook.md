# Gustav's Workbook

## PR #4939: Activate largest-gap optimistic elections first

* [x] Draw diagram
* [ ] Finish converting integration tests to unit test for optimistic scheduler
* [ ] Schedule highest gap first
* [ ] Reduce gap threshold and max size
* [ ] Add config option `optimistic_activation_delay`
* [ ] Add unit tests
* [ ] Remove/convert legacy tests
* [ ] Add documentation


## PR #4991: Log when peered stake is below quorum

* [ ] Add stale threshold to aec config
* [ ] Add stale count to AEC
* [ ] Log stale elections in monitor after warmup
* [ ] Log when peered stake is below quorum

## Backlog

* [ ] Why does bootstrap stall?
* [ ] Run a bootstrap with nano_node and compare
* [ ] Do vacancy calculation for schedulers inside AEC? Only wake up schedulers if AEC has vacancy?
