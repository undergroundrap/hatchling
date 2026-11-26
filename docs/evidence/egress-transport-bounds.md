# A cut opens nothing more, and a forward has a time of its own (#684)

Closes [#684](https://github.com/undergroundrap/hatchling/issues/684), three small things in `crates/hatchling-egress`'s transports from an outside review, confirmed in the code. None let anything out that shouldn't go.

## The fixes

1. **A cut is checked before connecting.**
   - **Before:** `local::forward` and `https::send_or_cut` connected, then held the connection for the cut, so a run cut while connecting waited up to `CONNECT_TIMEOUT` (5 s) for each pinned address.
   - **Now:** both `connect`s take the cut and check it before each address, so a cut run opens nothing more. The relay, which has no cut, passes one never cut.
2. **A forward has a total time.**
   - **Before:** `IO_TIMEOUT` bounded each read (30 s HTTPS, 300 s local), so an upstream trickling a byte at a time held a forward until the run's clock cut it.
   - **Now:** `within` wraps the response's reader, and a read once its time is past fails, so the forward ends as `UpstreamFailed`. One read can overshoot by its own timeout.
   - **The times:** `https::FORWARD_TIME` is 10 minutes. `local::FORWARD_TIME` is 20, room for a local model's long answer under a chat run's 30. A run with nobody there has 10, which end it first.
   - **Unchanged:** the broker's per-frame `ANSWER_TIMEOUT` (`local::IO_TIMEOUT` + 30 s) still waits past any one read, and the run's clock still cuts the forward when the run ends.
3. **One TLS connection builder.** `send_or_cut` calls `TlsClient::connection` instead of building its own.

## Tests

In `hatchling-egress`:
- `a_forward_cut_before_it_connects_opens_nothing` (local) and `an_https_forward_cut_before_it_connects_opens_nothing`: cut first, each forward fails, and the listener sees no connection come.
- `a_forward_cut_in_flight_ends_at_once_and_hangs_up_on_the_upstream`: its second half expected a cut forward to connect and send nothing. It now checks that nothing is opened.
- `a_trickling_answer_ends_at_the_forwards_time`:
  - an upstream giving a byte every 20 ms, given 300 ms, fails within 2 s;
  - an answer within its time is read whole;
  - a time already past reads nothing.
- The times are pinned with the other timeouts.

**What's covered by reading, not a test:** each forward passes its `FORWARD_TIME` to `within`. A test of it would wait the full 10 or 20 minutes; `within` itself is tested, and the times are pinned.

## The gate

RESULTS
