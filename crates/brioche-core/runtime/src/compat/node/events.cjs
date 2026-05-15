// Stub for node:events
class EventEmitter {
    on() { return this; }
    once() { return this; }
    off() { return this; }
    removeListener() { return this; }
    removeAllListeners() { return this; }
    emit() { return false; }
    addListener() { return this; }
    listenerCount() { return 0; }
    listeners() { return []; }
    setMaxListeners() { return this; }
}

module.exports = EventEmitter;
module.exports.EventEmitter = EventEmitter;
module.exports.default = EventEmitter;
