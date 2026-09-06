import { useEffect, useState } from 'react';
import type { WorkspaceContract } from '../types';
import { invoke } from '../lib/invoke';

export function useWorkspaceContract() {
  const [workspaceContract, setWorkspaceContract] = useState<WorkspaceContract | null>(null);
  const [workspaceContractError, setWorkspaceContractError] = useState('');

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      try {
        const contract = await invoke<WorkspaceContract>('workspace_contract');
        if (!cancelled) setWorkspaceContract(contract);
      } catch (error: any) {
        if (!cancelled) setWorkspaceContractError(String(error?.message ?? error));
      }
    };
    void load();
    return () => {
      cancelled = true;
    };
  }, []);

  return { workspaceContract, workspaceContractError };
}
