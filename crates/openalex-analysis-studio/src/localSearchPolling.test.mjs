import assert from 'node:assert/strict';
import test from 'node:test';
import { LOCAL_SEARCH_TASK_POLL_INTERVAL_MS, createLocalSearchTaskPoller } from './localSearchPolling.js';

async function flush() {
  await Promise.resolve();
  await Promise.resolve();
}

for (const terminalStatus of ['succeeded', 'failed', 'idle']) {
  test(`Search polling survives multiple intervals and converges after ${terminalStatus}`, async () => {
    const scheduled = [];
    const calls = [];
    const terminalRequests = [];
    const states = ['building', 'building', terminalStatus];
    let nextTimer = 0;
    const stop = createLocalSearchTaskPoller({
      request: { expectedGeneration: 12, expectedVaultPath: 'vault-b' },
      isCurrent: (request) => request.expectedGeneration === 12 && request.expectedVaultPath === 'vault-b',
      refresh: async (request) => {
        calls.push(request);
        return { status: states.shift() };
      },
      onTerminal: async (request) => { terminalRequests.push(request); },
      setTimer: (callback, delay) => {
        assert.equal(delay, LOCAL_SEARCH_TASK_POLL_INTERVAL_MS);
        const timer = nextTimer++;
        scheduled.push({ timer, callback, active: true });
        return timer;
      },
      clearTimer: (timer) => { scheduled[timer].active = false; },
    });

    for (let interval = 0; interval < 3; interval += 1) {
      const scheduledTimer = scheduled[interval];
      assert.ok(scheduledTimer?.active, `interval ${interval + 1} should be scheduled`);
      scheduledTimer.callback();
      await flush();
    }

    assert.equal(calls.length, 3, 'the task must be refreshed beyond the first 350 ms interval');
    assert.deepEqual(terminalRequests, [{ expectedGeneration: 12, expectedVaultPath: 'vault-b' }]);
    assert.equal(scheduled.length, 3, 'terminal state must stop further polling');
    stop();
  });
}

test('Search polling stops stale Vault writes before a later interval', async () => {
  const scheduled = [];
  let current = true;
  let refreshes = 0;
  let terminals = 0;
  createLocalSearchTaskPoller({
    request: { expectedGeneration: 3, expectedVaultPath: 'vault-a' },
    isCurrent: () => current,
    refresh: async () => { refreshes += 1; return { status: 'building' }; },
    onTerminal: async () => { terminals += 1; },
    setTimer: (callback) => { scheduled.push(callback); return scheduled.length - 1; },
    clearTimer: () => {},
  });

  scheduled[0]();
  await flush();
  current = false;
  scheduled[1]();
  await flush();
  assert.equal(refreshes, 1);
  assert.equal(terminals, 0);
});
