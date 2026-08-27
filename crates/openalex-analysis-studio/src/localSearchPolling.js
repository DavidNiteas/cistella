export const LOCAL_SEARCH_TASK_POLL_INTERVAL_MS = 350;

/**
 * Polls a Search task until its backend state reaches any non-building
 * terminal state. The caller owns all state writes and therefore retains the
 * connection-generation + Vault-path guard on every response.
 */
export function createLocalSearchTaskPoller({
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

    if (task?.status === 'building') {
      timer = setTimer(() => { void poll(); }, LOCAL_SEARCH_TASK_POLL_INTERVAL_MS);
      return;
    }

    await onTerminal(request);
  };

  timer = setTimer(() => { void poll(); }, LOCAL_SEARCH_TASK_POLL_INTERVAL_MS);
  return () => {
    stopped = true;
    if (timer !== null) clearTimer(timer);
  };
}
