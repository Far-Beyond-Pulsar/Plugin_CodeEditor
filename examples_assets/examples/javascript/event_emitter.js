// A tiny event emitter: a class, a Map of listeners and spread arguments.

class EventEmitter {
  #listeners = new Map();

  /** Calls `listener` every time `event` is emitted. Returns an unsubscribe function. */
  on(event, listener) {
    const list = this.#listeners.get(event) ?? [];
    list.push(listener);
    this.#listeners.set(event, list);
    return () => this.off(event, listener);
  }

  /** Calls `listener` once, on the next emit. */
  once(event, listener) {
    const off = this.on(event, (...args) => {
      off();
      listener(...args);
    });
    return off;
  }

  off(event, listener) {
    const list = this.#listeners.get(event) ?? [];
    this.#listeners.set(event, list.filter((l) => l !== listener));
  }

  emit(event, ...args) {
    // Copy so a listener that unsubscribes does not skip the next one.
    for (const listener of [...(this.#listeners.get(event) ?? [])]) {
      listener(...args);
    }
  }
}

const bus = new EventEmitter();
bus.once("score", (points) => console.log(`first score: ${points}`));
bus.on("score", (points) => console.log(`scored ${points}`));
bus.emit("score", 10);
bus.emit("score", 20);
