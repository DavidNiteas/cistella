import { useState } from 'react';
import type { Dict } from '../lib/i18n/dict';
import type { DocumentAsset, ReadingSession, ReadingSessionSummary, VaultConnection, VaultRequestContext } from '../types';
import { invoke } from '../lib/invoke';
import { useToast } from '../components/ui/Toast/ToastProvider';

export interface ReadingSessionsState {
  sessions: ReadingSessionSummary[];
  load: (request?: VaultRequestContext) => Promise<void>;
  start: (asset: DocumentAsset) => void;
  resume: (session: ReadingSession) => void;
  pause: (session: ReadingSession) => void;
  end: (session: ReadingSession) => void;
  continueReading: () => void;
}

export function useReadingSessions(vault: VaultConnection, t: Dict): ReadingSessionsState {
  const [sessions, setSessions] = useState<ReadingSessionSummary[]>([]);
  const toast = useToast();

  const runReadingAction = async (action: (request: VaultRequestContext) => Promise<void>) => {
    const request = vault.captureVaultRequest();
    if (!vault.isCurrentVaultRequest(request)) return;
    vault.setBusy(true);
    try {
      await action(request);
    } catch (e: any) {
      if (vault.isCurrentVaultRequest(request)) toast.push(`${t.failed}: ${e?.message ?? e}`, 'error');
    } finally {
      vault.setBusy(false);
    }
  };

  const load = async (request?: VaultRequestContext) => {
    const req = request ?? vault.captureVaultRequest();
    if (!req.expectedVaultPath) {
      if (vault.isCurrentVaultRequest(req)) setSessions([]);
      return;
    }
    try {
      const value = await invoke<ReadingSessionSummary[]>('list_recent_reading_sessions');
      if (vault.isCurrentVaultRequest(req)) {
        setSessions(Array.isArray(value) ? value : []);
      }
    } catch (e: any) {
      if (vault.isCurrentVaultRequest(req)) toast.push(`${t.failed}: ${e?.message ?? e}`, 'error');
    }
  };

  const start = (asset: DocumentAsset) => {
    void runReadingAction(async (request) => {
      await invoke('start_reading_session', { itemId: asset.itemId, assetId: asset.assetId });
      await load(request);
      toast.push(t.readingSessionOpened, 'info');
    });
  };

  const resume = (session: ReadingSession) => {
    void runReadingAction(async (request) => {
      await invoke('resume_reading_session', { itemId: session.itemId, assetId: session.assetId });
      await load(request);
      toast.push(t.readingSessionOpened, 'info');
    });
  };

  const pause = (session: ReadingSession) => {
    void runReadingAction(async (request) => {
      await invoke('pause_reading_session', { itemId: session.itemId, assetId: session.assetId });
      await load(request);
      toast.push(t.readingSessionUpdated, 'info');
    });
  };

  const end = (session: ReadingSession) => {
    void runReadingAction(async (request) => {
      await invoke('end_reading_session', { itemId: session.itemId, assetId: session.assetId });
      await load(request);
      toast.push(t.readingSessionUpdated, 'info');
    });
  };

  const continueReading = () => {
    void runReadingAction(async (request) => {
      await invoke('continue_reading_session');
      await load(request);
      toast.push(t.readingSessionOpened, 'info');
    });
  };

  return { sessions, load, start, resume, pause, end, continueReading };
}
