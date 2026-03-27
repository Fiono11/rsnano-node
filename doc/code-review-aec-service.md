# Code Review

* AEC got `insert_priority(block, prio)` instead of `insert(req: AecInsertRequest)` => is this a good idea?
* RPC `confirmatin_active` added `aec.confirmation_active()` => Remove that entirely?
    * uses `announcements` param which isn't even supported!
* `node.rs` hat `aec_delivery` bekommen. Why?
* new value type `AecSchedulerRequest` -> is the old `AecInsertRequest` still there? If yes, why the duplication?
