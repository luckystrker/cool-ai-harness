// Minimal executable OpenCode plugin entry used by the compatibility corpus.
// It is never executed by the tests: the worker protocol is exercised with the
// Bun-free stub binary, so CI does not need Bun installed.
export const plugin = {
  name: "opencode-demo",
  version: "0.1.0",
};

export default plugin;
