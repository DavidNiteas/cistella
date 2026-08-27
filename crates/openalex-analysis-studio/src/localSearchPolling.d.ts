export const LOCAL_SEARCH_TASK_POLL_INTERVAL_MS: number;

type SearchTaskLike = { status: string };

export function createLocalSearchTaskPoller<Request>(options: {
  request: Request;
  isCurrent: (request: Request) => boolean;
  refresh: (request: Request) => Promise<SearchTaskLike | null | undefined>;
  onTerminal: (request: Request) => Promise<void> | void;
  setTimer?: (callback: () => void, delay: number) => number;
  clearTimer?: (timer: number) => void;
}): () => void;
