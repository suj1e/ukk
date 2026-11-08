// First-party capability-limited plugin: no host globals, imports are denied.
export function invoke(request) {
  return {
    message: "hello from moejs",
    command: request.command,
    args: request.args ?? null,
    processType: typeof process,
    fetchType: typeof fetch,
  };
}
