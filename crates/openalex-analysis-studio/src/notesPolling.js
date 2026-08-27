export const NOTES_POLL_INTERVAL_MS = 400;

/**
 * Polls an asynchronous Notes operation until it reaches a terminal state.
 *
 * The caller owns all state writes, so every `refresh` and `onTerminal` call
 * is guarded by the same connection-generation + Vault-path snapshot.  This
 * makes it safe to use for annotation-resolution refresh or any other Notes
 * state that may take multiple intervals to settle.
 */
export function createNotesPoller({
  request,
  isCurrent,
  refresh,
  onTerminal,
  setTimer = window.setTimeout.bind(window),
  clearTimer = window.clearTimeout.bind(window),
}) {
  let stopped = false;
  let timer = null;

  const poll = async () => {
    if (stopped || !isCurrent(request)) return;
    const task = await refresh(request);
    if (stopped || !isCurrent(request)) return;

    if (task?.status === 'building' || task?.status === 'pending') {
      timer = setTimer(() => { void poll(); }, NOTES_POLL_INTERVAL_MS);
      return;
    }

    await onTerminal(request);
  };

  timer = setTimer(() => { void poll(); }, NOTES_POLL_INTERVAL_MS);
  return () => {
    stopped = true;
    if (timer !== null) clearTimer(timer);
  };
}
