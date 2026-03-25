# Gustav's Workbook

## Handshake Timeout

This adds a timeout for connections that are established but do not complete necessary node id handshake in time.

### Handshake Backlog

* [x] Create diagram for handshake process
* [ ] Create `HandshakeStatus` within `HandshakeProcess`
* [ ] HandshakeProcess is logic, but logs! => move logging
* [ ] Move handshake stats from `NanoDataReceiver` to `HandshakeProcess`
* [x] Add handshake_timeout config

## Backlog

* [ ] Why does bootstrap stall?
* [ ] Run a bootstrap with nano_node and compare
