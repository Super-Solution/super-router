import type {
  ControlPlane,
  PostgresStore,
} from "@supersolutions/super-router-storage-postgres";

export interface RecoveryWorkerOptions {
  /** Stop the loop when aborted. */
  signal?: AbortSignal;
  /** Delay between recovery passes, in milliseconds. Default 1000. */
  intervalMs?: number;
  /** Called with the number of requests recovered in a pass (only when > 0). */
  onRecovered?: (count: number) => void;
}

/**
 * Releases expired reservations, re-settles confirmed usage and advances
 * subscription periods until `signal` aborts. The caller owns the store and
 * closes it afterwards.
 */
export async function runRecoveryWorker(
  store: PostgresStore,
  control: ControlPlane,
  options: RecoveryWorkerOptions = {},
): Promise<void> {
  const { signal, intervalMs = 1000, onRecovered } = options;
  while (!signal?.aborted) {
    const n = await store.recover();
    await control.advancePeriods();
    if (n) onRecovered?.(n);
    await new Promise((resolve) => {
      const timer = setTimeout(resolve, intervalMs);
      signal?.addEventListener(
        "abort",
        () => {
          clearTimeout(timer);
          resolve(undefined);
        },
        { once: true },
      );
    });
  }
}
