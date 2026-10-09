type Action = { type: string; [key: string]: unknown };
let send: (message: unknown) => void;
let sequence = 0;
const pending = new Map<string, { resolve(): void; reject(error: Error): void }>();
let listening = false;

export function configureTransport(postMessage: (message: unknown) => void) {
  send = postMessage;
}

export function post(message: Action) { send(message); }

/** Complete only after the host persisted the action and refreshed its snapshot. */
export function request(message: Action): Promise<void> {
  if (!listening) {
    window.addEventListener("message", (event) => {
      const result = event.data;
      if (result?.type !== "operationResult") return;
      const operation = pending.get(result.requestId);
      if (!operation) return;
      pending.delete(result.requestId);
      if (result.error) operation.reject(new Error(result.error));
      else operation.resolve();
    });
    listening = true;
  }
  const requestId = `action-${Date.now()}-${++sequence}`;
  return new Promise<void>((resolve, reject) => {
    const timer = window.setTimeout(() => {
      pending.delete(requestId);
      reject(new Error("This action is taking longer than expected. Refresh to check whether it completed."));
    }, 90_000);
    pending.set(requestId, {
      resolve: () => { clearTimeout(timer); resolve(); },
      reject: (error) => { clearTimeout(timer); reject(error); },
    });
    try { send({ ...message, requestId }); }
    catch (error) {
      const operation = pending.get(requestId);
      pending.delete(requestId);
      operation?.reject(error instanceof Error ? error : new Error(String(error)));
    }
  });
}
