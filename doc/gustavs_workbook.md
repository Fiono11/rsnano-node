# Gustav's Workbook

## Handshake Timeout

This adds a timeout for connections that are established but do not complete necessary node id handshake in time.

### Backlog

* [x] Create diagram for handshake process
* [ ] Create `HandshakeStatus` within `HandshakeProcess`
* [ ] HandshakeProcess is logic, but logs! => move logging
* [ ] Move handshake stats from `NanoDataReceiver` to `HandshakeProcess`
* [ ] Add handshake_timeout config
