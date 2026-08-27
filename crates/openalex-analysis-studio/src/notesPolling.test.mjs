import assert from 'node:assert/strict';
import test from 'node:test';
import { NOTES_POLL_INTERVAL_MS, createNotesPoller } from './notesPolling.js';

async function flush() {
  await Promise.resolve();
  await Promise.resolve();
}

for (const terminalStatus of ['resolved_exact', 'failed', 'idle']) {
  test(`Notes polling survives multiple intervals and converges after ${terminalStatus}`, async () => {
    const scheduled = [];
    const calls = [];
    const terminalRequests = [];
    const states = ['pending', 'pending', terminalStatus];
    let nextTimer = 0;
    const stop = createNotesPoller({
      request: { expectedGeneration: 7, expectedVaultPath: 'vault-notes' },
      isCurrent: (request) => request.expectedGeneration === 7 && request.expectedVaultPath === 'vault-notes',
      refresh: async (request) => {
        calls.push(request);
        return { status: states.shift() };
      },
      onTerminal: async (request) => { terminalRequests.push(request); },
      setTimer: (callback, delay) => {
        assert.equal(delay, NOTES_POLL_INTERVAL_MS);
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

    assert.equal(calls.length, 3, 'the operation must be refreshed beyond the first interval');
    assert.deepEqual(terminalRequests, [{ expectedGeneration: 7, expectedVaultPath: 'vault-notes' }]);
    assert.equal(scheduled.length, 3, 'terminal state must stop further polling');
    stop();
  });
}

test('Notes polling stops stale Vault writes before a later interval', async () => {
  const scheduled = [];
  let current = true;
  let refreshes = 0;
  let terminals = 0;
  createNotesPoller({
    request: { expectedGeneration: 4, expectedVaultPath: 'vault-a' },
    isCurrent: () => current,
    refresh: async () => { refreshes += 1; return { status: 'pending' }; },
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
