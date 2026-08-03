# A dead SSE listener blocks chat for every later listener on the same stream

## Summary

When a client's connection to `GET /live/{id}/events` dies without the
`ReadableStream`'s `cancel()` running (a half-open TCP flow — the usual NAT or
router idle timeout — leaves no FIN and no RST), its five `streamingService`
handlers stay registered forever. Because `send()` is not guarded and Node's
`EventEmitter.emit()` runs handlers synchronously in registration order,
stopping at the first one that throws, that stale listener then blocks chat
delivery for **every listener registered after it on the same stream**, and
makes `DELETE /live/{id}` return 500.

## The chain

In `src/routes/live/[id]/events/+server.ts`:

```js
function send(eventName, data) {
    if (!controllerRef) return;
    controllerRef.enqueue(encoder.encode(payload));   // not guarded
}

keepalive = setInterval(() => {
    try { controllerRef?.enqueue(encoder.encode(": keepalive\n\n")); }
    catch { clearInterval(keepalive ?? undefined); keepalive = null; }
}, 30000);
```

1. The transport dies without `cancel()` running, so the `streamingService.on(...)`
   handlers registered in `start()` are never removed and `listenerDisconnected`
   is never called.
2. Within 30 s the keepalive `enqueue` throws. Its catch **clears the interval
   but leaves the handlers registered** — the server notices the connection is
   dead and then forgets about it.
3. Any later `emit` reaches that listener's `onChatSent` / `onStateChanged`,
   which call the unguarded `send()` → `enqueue` on a closed controller →
   throws.

## Consequences

- **Chat stops for other listeners.** `emit` aborts at the throwing handler, so
  every listener that registered after the dead one gets nothing. Reconnecting
  does not help: a new connection registers *after* the stale one.
- **`POST /live/{id}` returns 500 to the sender**, after the row has already
  been written — so the message is stored but reported as failed.
- **`DELETE /live/{id}` returns 500.** `notifyStateChanged` inside
  `endStream()` hits the same handler and throws. The route itself only ever
  intends 401/403/404/400.
- **Archiving is silently skipped.** That `notifyStateChanged` call sits
  *before* the `shouldArchive` block in `endStream()`, so the throw means
  `ffmpeg`, `Audio.create` and `STREAM_ARCHIVED` never run — while the stream
  row has already been set to `finished` and the source killed.

## Evidence

Reported by a Pubsplash user (an accessibility-first desktop streaming client
for Audio Pub). Chat stopped arriving partway through a broadcast while audio
kept flowing, and only stopping and restarting the stream restored it — which
fits: the handlers filter on `data.streamId === stream.id`, so a new stream id
makes the stale handler's condition false and it stops throwing.

Their logs show `DELETE /live/{id}` returning 500 at the end of three long
streams (1h12m, 1h28m, 1h35m) and not on a short one.

## Suggested fix

1. Wrap `send()` in the same try/catch the keepalive has.
2. On any `enqueue` failure — in `send()` or the keepalive — run the same
   teardown `cancel()` does: `off()` all five handlers and call
   `streamingService.listenerDisconnected(stream.id)`. Clearing the interval
   alone leaves the listener able to poison later emits.
3. Consider moving `notifyStateChanged` in `endStream()` after the archive
   block, or wrapping it, so a listener problem cannot cost a user their
   recording.

A defensive `try/catch` around each handler invocation (or emitting
asynchronously) would also stop one listener's failure from affecting others.
