// Stub for node:worker_threads
module.exports.isMainThread = true;
module.exports.threadId = 0;
module.exports.SHARE_ENV = Symbol("SHARE_ENV");
module.exports.Worker = class Worker {
  constructor() {
    throw new Error("Worker threads not available");
  }
};
